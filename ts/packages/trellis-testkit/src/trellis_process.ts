import { spawn, type ChildProcessByStdio } from "node:child_process";
import { once } from "node:events";
import { open, readdir, readFile, type FileHandle } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Readable } from "node:stream";
import { setTimeout as delay } from "node:timers/promises";

import {
  type ProcessIdentity,
  processMatchesIdentity,
  recordProcessIdentity,
} from "./cleanup.ts";
import type { ReservedPort } from "./ports.ts";

const DEFAULT_OUTPUT_TAIL_CHARS = 8_192;
const READINESS_POLL_INTERVAL_MS = 25;
const READINESS_FETCH_TIMEOUT_MS = 250;
const GROUP_EXIT_TIMEOUT_MS = 10_000;
const GROUP_EXIT_POLL_MS = 10;

/** @internal Resolved status of a spawned child process. */
export type CommandStatus = {
  code: number | null;
  signal: NodeJS.Signals | null;
};

type TrellisChild = ChildProcessByStdio<null, Readable, Readable>;

type BootstrapUrlWaiter = {
  resolve(value: string): void;
  reject(error: Error): void;
  timeoutId: ReturnType<typeof setTimeout>;
};

/** @internal Handle for a spawned Trellis control-plane process. */
export type TrellisProcessHandle = {
  readonly trellisUrl: string;
  readonly bootstrapUrl: string | undefined;
  waitForBootstrapUrl(timeoutMs: number): Promise<string>;
  outputTails(): string;
  stop(): Promise<void>;
  [Symbol.asyncDispose](): Promise<void>;
};

/** @internal Resolved local command used to spawn Trellis. */
export type ResolvedTrellisProcessCommand = {
  readonly cmd: string;
  readonly args: readonly string[];
  readonly env?: Record<string, string>;
  readonly cwd?: string;
  /** The command creates its own Unix process group with its PID as the group ID. */
  readonly isolatedProcessGroup?: boolean;
};

/** @internal Arguments for starting a spawned Trellis process. */
export type StartTrellisProcessArgs = {
  trellisUrl: string;
  configPath: string;
  command: ResolvedTrellisProcessCommand;
  startupTimeoutMs: number;
  shutdownTimeoutMs: number;
  /** Ports handed to the child at spawn; the caller keeps owning them. */
  portLeases?: readonly ReservedPort[];
  /**
   * The pid file the host writes for the NATS broker it manages. The broker
   * runs in a process group of its own, so stopping waits for it separately.
   */
  brokerPidFile?: string;
};

/** @internal Bounded text buffer for child process output tails. */
export class TextTail {
  #text = "";

  private readonly maxChars: number;

  constructor(maxChars: number) {
    this.maxChars = maxChars;
  }

  append(text: string): void {
    this.#text += text;
    if (this.#text.length > this.maxChars) {
      this.#text = this.#text.slice(-this.maxChars);
    }
  }

  toString(): string {
    return this.#text;
  }
}

// A test that times out never reaches its cleanup, and the test runner then
// exits the process. Whatever is still running goes with it.
const runningHosts = new Set<{ child: TrellisChild; group?: number }>();
process.once("exit", () => {
  for (const { child, group } of runningHosts) {
    killProcess(child, "SIGKILL");
    if (group === undefined) continue;
    try {
      process.kill(-group, "SIGKILL");
    } catch {
      // The group is already gone.
    }
  }
});

class StartedTrellisProcess implements TrellisProcessHandle {
  readonly trellisUrl: string;
  readonly #child: TrellisChild;
  readonly #processId: number;
  readonly #status: Promise<CommandStatus>;
  readonly #stdoutReader: Promise<void>;
  readonly #stderrReader: Promise<void>;
  readonly #shutdownTimeoutMs: number;
  readonly #isolatedProcessGroup: boolean;
  readonly #brokerPidFile: string | undefined;
  #broker: ProcessIdentity | undefined;
  readonly #stdoutTail: TextTail;
  readonly #stderrTail: TextTail;
  #bootstrapUrl: string | undefined;
  #bootstrapWaiters: BootstrapUrlWaiter[] = [];
  #stopping: Promise<void> | undefined;
  readonly #running: { child: TrellisChild; group?: number };

  constructor(args: {
    trellisUrl: string;
    child: TrellisChild;
    processId: number;
    status: Promise<CommandStatus>;
    stdoutReader: Promise<void>;
    stderrReader: Promise<void>;
    stdoutTail: TextTail;
    stderrTail: TextTail;
    shutdownTimeoutMs: number;
    isolatedProcessGroup: boolean;
    brokerPidFile: string | undefined;
  }) {
    this.trellisUrl = args.trellisUrl;
    this.#child = args.child;
    this.#processId = args.processId;
    this.#status = args.status;
    this.#stdoutReader = args.stdoutReader;
    this.#stderrReader = args.stderrReader;
    this.#stdoutTail = args.stdoutTail;
    this.#stderrTail = args.stderrTail;
    this.#shutdownTimeoutMs = args.shutdownTimeoutMs;
    this.#isolatedProcessGroup = args.isolatedProcessGroup;
    this.#brokerPidFile = args.brokerPidFile;
    this.#running = {
      child: args.child,
      ...(args.isolatedProcessGroup ? { group: args.processId } : {}),
    };
    runningHosts.add(this.#running);
  }

  get bootstrapUrl(): string | undefined {
    return this.#bootstrapUrl;
  }

  recordBootstrapUrl(url: string): void {
    if (this.#bootstrapUrl !== undefined) return;
    this.#bootstrapUrl = url;
    const waiters = this.#bootstrapWaiters.splice(0);
    for (const waiter of waiters) {
      clearTimeout(waiter.timeoutId);
      waiter.resolve(url);
    }
  }

  waitForBootstrapUrl(timeoutMs: number): Promise<string> {
    if (this.#bootstrapUrl !== undefined) {
      return Promise.resolve(this.#bootstrapUrl);
    }
    return new Promise((resolve, reject) => {
      const waiter: BootstrapUrlWaiter = {
        resolve,
        reject,
        timeoutId: setTimeout(() => {
          const index = this.#bootstrapWaiters.indexOf(waiter);
          if (index >= 0) this.#bootstrapWaiters.splice(index, 1);
          reject(
            new Error(
              `Timed out after ${timeoutMs}ms waiting for Trellis admin bootstrap URL`,
            ),
          );
        }, timeoutMs),
      };
      this.#bootstrapWaiters.push(waiter);
    });
  }

  outputTails(): string {
    return processOutputTails({
      stdoutTail: this.#stdoutTail,
      stderrTail: this.#stderrTail,
    });
  }

  /** Records the managed broker while the host that started it is running. */
  async recordBroker(): Promise<void> {
    if (this.#brokerPidFile === undefined || this.#broker !== undefined) return;
    this.#broker = await readBrokerIdentity(this.#brokerPidFile);
  }

  stop(): Promise<void> {
    this.#stopping ??= this.#stopOnce();
    return this.#stopping;
  }

  async #stopOnce(): Promise<void> {
    // A host that crashed before readiness leaves its broker's pid file.
    await this.recordBroker();
    const alreadyExited = await settledStatus(this.#status);
    if (alreadyExited === undefined) {
      killProcess(this.#child, "SIGTERM");
      const terminated = await waitForStatus(
        this.#status,
        this.#shutdownTimeoutMs,
      );
      if (terminated === undefined) {
        console.error(
          `Trellis host ${this.#child.pid} exceeded its ${this.#shutdownTimeoutMs}ms shutdown bound; sending SIGKILL`,
        );
        console.error(this.outputTails());
        killProcess(this.#child, "SIGKILL");
        await this.#status.catch(() => undefined);
      }
    }

    // A killed or crashed host cannot run its managed-child destructors. Its
    // dedicated group remains ours even after the leader exits. Also reclaim
    // descendants before awaiting EOF on any inherited output pipes.
    // SIGKILL is delivered asynchronously: wait until the group is gone, so
    // a broker still writing its store can't race the sandbox removal. A
    // zombie writes nothing, and an orphan's may never be reaped in a
    // container without an init, so on Linux only living members count.
    if (this.#isolatedProcessGroup) {
      const deadline = Date.now() + GROUP_EXIT_TIMEOUT_MS;
      let signal: NodeJS.Signals | 0 = "SIGKILL";
      while (true) {
        try {
          process.kill(-this.#processId, signal);
        } catch (error) {
          if (
            error instanceof Error &&
            "code" in error &&
            error.code === "ESRCH"
          )
            break;
          throw error;
        }
        if (!(await groupHasLivingMember(this.#processId))) break;
        if (Date.now() >= deadline) {
          throw new Error(
            `Trellis process group ${this.#processId} still had members ${GROUP_EXIT_TIMEOUT_MS}ms after SIGKILL`,
          );
        }
        signal = 0;
        await delay(GROUP_EXIT_POLL_MS);
      }
    }
    // The broker has a group of its own, so a Ctrl-C in the host's terminal
    // reaches only the host. A host stopped in order stops it first; a
    // killed host's broker gets SIGTERM from the kernel and may still be
    // writing its store, so wait for it before the sandbox goes.
    if (this.#broker !== undefined) {
      const broker = this.#broker;
      let deadline = Date.now() + GROUP_EXIT_TIMEOUT_MS;
      let killed = false;
      while (
        (await processMatchesIdentity(broker)) &&
        (await processHasLivingTask(broker.pid))
      ) {
        if (Date.now() >= deadline) {
          if (killed) {
            throw new Error(
              `Managed NATS broker ${broker.pid} still ran ${GROUP_EXIT_TIMEOUT_MS}ms after SIGKILL`,
            );
          }
          console.error(
            `Managed NATS broker ${broker.pid} outlived its host by ${GROUP_EXIT_TIMEOUT_MS}ms; sending SIGKILL`,
          );
          try {
            process.kill(broker.pid, "SIGKILL");
          } catch {
            // It exited after the last check.
          }
          killed = true;
          deadline += GROUP_EXIT_TIMEOUT_MS;
        }
        await delay(GROUP_EXIT_POLL_MS);
      }
    }
    runningHosts.delete(this.#running);
    await Promise.allSettled([this.#stdoutReader, this.#stderrReader]);
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.stop();
  }
}

function timeoutValue<T>(
  ms: number,
  value: T,
): {
  promise: Promise<T>;
  cancel(): void;
} {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const promise = new Promise<T>((resolve) => {
    timeoutId = setTimeout(() => {
      timeoutId = undefined;
      resolve(value);
    }, ms);
  });
  return {
    promise,
    cancel() {
      if (timeoutId !== undefined) clearTimeout(timeoutId);
    },
  };
}

/**
 * Reports whether process group `groupId` has a member with a thread that
 * isn't a zombie. A killed multithreaded process, such as the broker, shows a
 * zombie main thread while its other threads still finish their writes.
 * Without `/proc` every member counts, so only an empty group is gone.
 */
async function groupHasLivingMember(groupId: number): Promise<boolean> {
  let entries: string[];
  try {
    entries = await readdir("/proc");
  } catch {
    return true;
  }
  for (const entry of entries) {
    if (!/^\d+$/.test(entry)) continue;
    const pgrp = (await statFields(`/proc/${entry}/stat`))?.[2];
    if (Number(pgrp) !== groupId) continue;
    if (await livingTask(entry)) return true;
  }
  return false;
}

/**
 * Reports whether process `pid` has a thread that isn't a zombie, counted as
 * {@link groupHasLivingMember} counts them. Without `/proc` a process that
 * exists counts as living.
 */
async function processHasLivingTask(pid: number): Promise<boolean> {
  if (!(await hasProc())) {
    try {
      process.kill(pid, 0);
      return true;
    } catch {
      return false;
    }
  }
  return await livingTask(String(pid));
}

async function hasProc(): Promise<boolean> {
  try {
    await readdir("/proc");
    return true;
  } catch {
    return false;
  }
}

// `pid (comm) state ppid pgrp ...`; comm may itself contain spaces or `)`.
async function statFields(path: string): Promise<string[] | undefined> {
  try {
    const stat = await readFile(path, "utf8");
    return stat.slice(stat.lastIndexOf(")") + 2).split(" ");
  } catch {
    return undefined;
  }
}

async function livingTask(pid: string): Promise<boolean> {
  let tasks: string[];
  try {
    tasks = await readdir(`/proc/${pid}/task`);
  } catch {
    return false;
  }
  for (const task of tasks) {
    const state = (await statFields(`/proc/${pid}/task/${task}/stat`))?.[0];
    if (state !== undefined && state !== "Z" && state !== "X") return true;
  }
  return false;
}

/**
 * Reads the broker's pid file and records the process it names, if that is
 * a nats-server: a file a broker left behind may name a reused pid. Without
 * `/proc` the pid file is all there is to go on.
 */
async function readBrokerIdentity(
  pidFile: string,
): Promise<ProcessIdentity | undefined> {
  let pid: number;
  try {
    pid = Number((await readFile(pidFile, "utf8")).trim());
  } catch {
    return undefined;
  }
  if (!Number.isSafeInteger(pid) || pid <= 0) return undefined;
  if (await hasProc()) {
    const comm = await readFile(`/proc/${pid}/comm`, "utf8").catch(() => "");
    if (!comm.startsWith("nats-server")) return undefined;
  }
  return await recordProcessIdentity(pid, "");
}

/** @internal Resolves the status if the child already exited, else undefined. */
export async function settledStatus(
  status: Promise<CommandStatus>,
): Promise<CommandStatus | undefined> {
  const timeout = timeoutValue(0, undefined);
  try {
    return await Promise.race([status, timeout.promise]);
  } finally {
    timeout.cancel();
  }
}

/** @internal Waits up to `timeoutMs` for the child status to resolve. */
export async function waitForStatus(
  status: Promise<CommandStatus>,
  timeoutMs: number,
): Promise<CommandStatus | undefined> {
  const timeout = timeoutValue(timeoutMs, undefined);
  try {
    return await Promise.race([status, timeout.promise]);
  } finally {
    timeout.cancel();
  }
}

/** @internal Sends a signal, ignoring races where the child already exited. */
export function killProcess(child: TrellisChild, signal: NodeJS.Signals): void {
  try {
    child.kill(signal);
  } catch {
    // The process may have exited between the status check and signal delivery.
  }
}

/** @internal Human-readable exit description for a resolved child status. */
export function commandStatusText(status: CommandStatus): string {
  if (status.signal !== null) return `signal ${status.signal}`;
  return `exit code ${status.code}`;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** @internal Parses the startup account URL from one structured output line. */
export function parseTrellisBootstrapUrl(line: string): string | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (!isRecord(parsed)) return undefined;
  const value = parsed.adminAccountUrl;
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** @internal Pipes child output to a log file while tracking a text tail. */
export async function captureProcessOutput(
  stream: AsyncIterable<Uint8Array>,
  tail: TextTail,
  onLine: (line: string) => void,
  log: FileHandle,
): Promise<void> {
  const decoder = new TextDecoder();
  let pending = "";
  for await (const chunk of stream) {
    await log.writeFile(chunk);
    const text = decoder.decode(chunk, { stream: true });
    tail.append(text);
    pending += text;
    let newlineIndex = pending.indexOf("\n");
    while (newlineIndex >= 0) {
      const line = pending.slice(0, newlineIndex).replace(/\r$/, "");
      onLine(line);
      if (process.env.TRELLIS_TEST_TRACE_OUTPUT === "1") {
        console.error(line);
      }
      pending = pending.slice(newlineIndex + 1);
      newlineIndex = pending.indexOf("\n");
    }
  }
  const remaining = decoder.decode();
  if (remaining.length > 0) {
    tail.append(remaining);
    pending += remaining;
  }
  if (pending.length > 0) onLine(pending.replace(/\r$/, ""));
}

function processOutputTails(args: {
  stdoutTail: TextTail;
  stderrTail: TextTail;
}): string {
  const stdout = args.stdoutTail.toString().trimEnd() || "<empty>";
  const stderr = args.stderrTail.toString().trimEnd() || "<empty>";
  return `stdout tail:\n${stdout}\nstderr tail:\n${stderr}`;
}

function versionUrl(trellisUrl: string): string {
  return `${trellisUrl.replace(/\/+$/, "")}/readyz`;
}

async function fetchReady(
  url: string,
  timeoutMs: number,
  processId: number,
): Promise<boolean> {
  const abort = new AbortController();
  const timeoutId = setTimeout(() => abort.abort(), timeoutMs);
  try {
    const response = await fetch(url, { signal: abort.signal });
    const metadata: unknown = await response.json();
    return (
      response.ok && isRecord(metadata) && metadata.processId === processId
    );
  } finally {
    clearTimeout(timeoutId);
  }
}

async function waitForTrellisReady(args: {
  processId: number;
  trellisUrl: string;
  startupTimeoutMs: number;
  status: Promise<CommandStatus>;
  stdoutTail: TextTail;
  stderrTail: TextTail;
  readers: readonly Promise<void>[];
}): Promise<void> {
  const url = versionUrl(args.trellisUrl);
  const startedAt = Date.now();

  while (Date.now() - startedAt <= args.startupTimeoutMs) {
    const status = await settledStatus(args.status);
    if (status !== undefined) {
      await Promise.allSettled(args.readers);
      throw new Error(
        `Trellis process exited before readiness (${commandStatusText(
          status,
        )}) while polling ${url}\n${processOutputTails(args)}`,
      );
    }

    const remainingMs = args.startupTimeoutMs - (Date.now() - startedAt);
    if (remainingMs <= 0) break;

    try {
      const fetchTimeoutMs = Math.min(READINESS_FETCH_TIMEOUT_MS, remainingMs);
      if (await fetchReady(url, fetchTimeoutMs, args.processId)) return;
    } catch {
      // Keep polling until the process exits or the startup deadline expires.
    }

    const pollDelayMs = Math.min(
      READINESS_POLL_INTERVAL_MS,
      Math.max(0, args.startupTimeoutMs - (Date.now() - startedAt)),
    );
    const timeout = timeoutValue(pollDelayMs, undefined);
    const exitDuringDelay = await Promise.race([
      args.status.then((status) => ({ status })),
      timeout.promise,
    ]).finally(() => timeout.cancel());
    if (exitDuringDelay !== undefined) {
      await Promise.allSettled(args.readers);
      throw new Error(
        `Trellis process exited before readiness (${commandStatusText(
          exitDuringDelay.status,
        )}) while polling ${url}\n${processOutputTails(args)}`,
      );
    }
  }

  throw new Error(
    `Timed out after ${args.startupTimeoutMs}ms waiting for Trellis process readiness at ${url}\n${processOutputTails(
      args,
    )}`,
  );
}

/** @internal Starts a spawned Trellis control-plane process. */
export async function startTrellisProcess(
  args: StartTrellisProcessArgs,
): Promise<TrellisProcessHandle> {
  const command = args.command;
  const stdoutTail = new TextTail(DEFAULT_OUTPUT_TAIL_CHARS);
  const stderrTail = new TextTail(DEFAULT_OUTPUT_TAIL_CHARS);
  const logDir = dirname(args.configPath);
  const stdoutLog = await open(join(logDir, "trellis.stdout.log"), "w", 0o600);
  const stderrLog = await open(
    join(logDir, "trellis.stderr.log"),
    "w",
    0o600,
  ).catch(async (error) => {
    await stdoutLog.close();
    throw error;
  });

  let child: TrellisChild;
  try {
    for (const lease of args.portLeases ?? []) await lease.releaseForSpawn();
    child = spawn(command.cmd, [...command.args], {
      cwd: command.cwd,
      env: command.env ?? {},
      stdio: ["ignore", "pipe", "pipe"],
    });
    await once(child, "spawn");
  } catch (error) {
    await Promise.all([stdoutLog.close(), stderrLog.close()]);
    throw error;
  }
  const status = new Promise<CommandStatus>((resolve, reject) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
    child.once("error", reject);
  });
  const processId = child.pid;
  if (processId === undefined)
    throw new Error("Spawned Trellis process has no pid");

  let handle: StartedTrellisProcess;
  const recordBootstrapLine = (line: string) => {
    const bootstrapUrl = parseTrellisBootstrapUrl(line);
    if (bootstrapUrl !== undefined) handle.recordBootstrapUrl(bootstrapUrl);
  };
  const stdoutReader = captureProcessOutput(
    child.stdout,
    stdoutTail,
    recordBootstrapLine,
    stdoutLog,
  )
    .catch((error) => {
      stdoutTail.append(`\n<failed to read stdout: ${String(error)}>\n`);
    })
    .finally(async () => {
      await stdoutLog.close();
    });
  const stderrReader = captureProcessOutput(
    child.stderr,
    stderrTail,
    recordBootstrapLine,
    stderrLog,
  )
    .catch((error) => {
      stderrTail.append(`\n<failed to read stderr: ${String(error)}>\n`);
    })
    .finally(async () => {
      await stderrLog.close();
    });

  handle = new StartedTrellisProcess({
    trellisUrl: args.trellisUrl,
    child,
    processId,
    status,
    stdoutReader,
    stderrReader,
    stdoutTail,
    stderrTail,
    shutdownTimeoutMs: args.shutdownTimeoutMs,
    isolatedProcessGroup: args.command.isolatedProcessGroup ?? false,
    brokerPidFile: args.brokerPidFile,
  });

  try {
    await waitForTrellisReady({
      processId,
      trellisUrl: args.trellisUrl,
      startupTimeoutMs: args.startupTimeoutMs,
      status,
      stdoutTail,
      stderrTail,
      readers: [stdoutReader, stderrReader],
    });
    await handle.recordBroker();
    return handle;
  } catch (error) {
    try {
      await handle.stop();
    } catch (cleanupError) {
      throw new AggregateError(
        [error, cleanupError],
        "Trellis process startup failed and cleanup was incomplete",
      );
    }
    throw error;
  }
}
