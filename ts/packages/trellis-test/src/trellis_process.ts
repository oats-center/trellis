import { dirname, join } from "@std/path";

import type { ReservedPort } from "./control_plane_config.ts";
import type { TrellisTestRuntimeStartOptions } from "./types.ts";

const DEFAULT_OUTPUT_TAIL_CHARS = 8_192;
const READINESS_POLL_INTERVAL_MS = 25;
const READINESS_FETCH_TIMEOUT_MS = 250;

/** @internal Resolved status of a spawned child process. */
export type CommandStatus = Awaited<Deno.ChildProcess["status"]>;

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
};

/** @internal Arguments for starting a spawned Trellis process. */
export type StartTrellisProcessArgs = {
  trellisUrl: string;
  configPath: string;
  options: TrellisTestRuntimeStartOptions["trellis"] | undefined;
  startupTimeoutMs: number;
  shutdownTimeoutMs: number;
  portLease?: ReservedPort;
};

/** @internal Bounded text buffer for child process output tails. */
export class TextTail {
  #text = "";

  constructor(private readonly maxChars: number) {}

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

class StartedTrellisProcess implements TrellisProcessHandle {
  readonly trellisUrl: string;
  readonly #child: Deno.ChildProcess;
  readonly #status: Promise<CommandStatus>;
  readonly #stdoutReader: Promise<void>;
  readonly #stderrReader: Promise<void>;
  readonly #shutdownTimeoutMs: number;
  readonly #stdoutTail: TextTail;
  readonly #stderrTail: TextTail;
  #bootstrapUrl: string | undefined;
  #bootstrapWaiters: BootstrapUrlWaiter[] = [];
  #stopping: Promise<void> | undefined;

  constructor(
    args: {
      trellisUrl: string;
      child: Deno.ChildProcess;
      status: Promise<CommandStatus>;
      stdoutReader: Promise<void>;
      stderrReader: Promise<void>;
      stdoutTail: TextTail;
      stderrTail: TextTail;
      shutdownTimeoutMs: number;
    },
  ) {
    this.trellisUrl = args.trellisUrl;
    this.#child = args.child;
    this.#status = args.status;
    this.#stdoutReader = args.stdoutReader;
    this.#stderrReader = args.stderrReader;
    this.#stdoutTail = args.stdoutTail;
    this.#stderrTail = args.stderrTail;
    this.#shutdownTimeoutMs = args.shutdownTimeoutMs;
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

  stop(): Promise<void> {
    this.#stopping ??= this.#stopOnce();
    return this.#stopping;
  }

  async #stopOnce(): Promise<void> {
    const alreadyExited = await settledStatus(this.#status);
    if (alreadyExited === undefined) {
      killProcess(this.#child, "SIGTERM");
      const terminated = await waitForStatus(
        this.#status,
        this.#shutdownTimeoutMs,
      );
      if (terminated === undefined) {
        killProcess(this.#child, "SIGKILL");
        await this.#status.catch(() => undefined);
      }
    }

    await Promise.allSettled([this.#stdoutReader, this.#stderrReader]);
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.stop();
  }
}

function timeoutValue<T>(ms: number, value: T): {
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
export function killProcess(
  child: Deno.ChildProcess,
  signal: Deno.Signal,
): void {
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

function jsonBootstrapUrl(line: string): string | undefined {
  let parsed: unknown;
  try {
    parsed = JSON.parse(line);
  } catch {
    return undefined;
  }
  if (!isRecord(parsed)) return undefined;
  const value = parsed.adminAccountUrl ?? parsed.bootstrapUrl ??
    (isRecord(parsed.fields)
      ? parsed.fields.adminAccountUrl ?? parsed.fields.bootstrapUrl
      : undefined);
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

function fallbackBootstrapUrl(line: string): string | undefined {
  const match = line.match(/\bTRELLIS_ADMIN_BOOTSTRAP_URL=(\S+)/);
  return match?.[1];
}

/** @internal Parses a bootstrap URL from one Trellis log line. */
export function parseTrellisBootstrapUrl(line: string): string | undefined {
  return jsonBootstrapUrl(line) ?? fallbackBootstrapUrl(line);
}

/** @internal Pipes child output to a log file while tracking a text tail. */
export async function captureProcessOutput(
  stream: ReadableStream<Uint8Array>,
  tail: TextTail,
  onLine: (line: string) => void,
  log: Deno.FsFile,
): Promise<void> {
  const decoder = new TextDecoder();
  let pending = "";
  for await (const chunk of stream) {
    await log.write(chunk);
    const text = decoder.decode(chunk, { stream: true });
    tail.append(text);
    pending += text;
    let newlineIndex = pending.indexOf("\n");
    while (newlineIndex >= 0) {
      const line = pending.slice(0, newlineIndex).replace(/\r$/, "");
      onLine(line);
      if (Deno.env.get("TRELLIS_TEST_TRACE_OUTPUT") === "1") {
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

async function fetchReady(url: string, timeoutMs: number): Promise<boolean> {
  const abort = new AbortController();
  const timeoutId = setTimeout(() => abort.abort(), timeoutMs);
  try {
    const response = await fetch(url, { signal: abort.signal });
    return response.ok;
  } finally {
    clearTimeout(timeoutId);
  }
}

async function waitForTrellisReady(args: {
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
        `Trellis process exited before readiness (${
          commandStatusText(status)
        }) while polling ${url}\n${processOutputTails(args)}`,
      );
    }

    const remainingMs = args.startupTimeoutMs - (Date.now() - startedAt);
    if (remainingMs <= 0) break;

    try {
      const fetchTimeoutMs = Math.min(READINESS_FETCH_TIMEOUT_MS, remainingMs);
      if (await fetchReady(url, fetchTimeoutMs)) return;
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
        `Trellis process exited before readiness (${
          commandStatusText(exitDuringDelay.status)
        }) while polling ${url}\n${processOutputTails(args)}`,
      );
    }
  }

  throw new Error(
    `Timed out after ${args.startupTimeoutMs}ms waiting for Trellis process readiness at ${url}\n${
      processOutputTails(args)
    }`,
  );
}

/** @internal Resolves the command used to spawn the Trellis control-plane process. */
export function resolveTrellisProcessCommand(
  options: TrellisTestRuntimeStartOptions["trellis"] | undefined,
): ResolvedTrellisProcessCommand {
  if (options?.command === undefined) {
    throw new Error("TrellisTestRuntime.start requires trellis.command");
  }

  return {
    cmd: options.command.cmd,
    args: options.command.args,
    env: options.command.env,
    cwd: options.command.cwd,
  };
}

/** @internal Starts a spawned Trellis control-plane process. */
export async function startTrellisProcess(
  args: StartTrellisProcessArgs,
): Promise<TrellisProcessHandle> {
  const command = resolveTrellisProcessCommand(args.options);
  const stdoutTail = new TextTail(DEFAULT_OUTPUT_TAIL_CHARS);
  const stderrTail = new TextTail(DEFAULT_OUTPUT_TAIL_CHARS);
  const logDir = dirname(args.configPath);
  const stdoutLog = await Deno.open(join(logDir, "trellis.stdout.log"), {
    write: true,
    create: true,
    truncate: true,
  });
  const stderrLog = await Deno.open(join(logDir, "trellis.stderr.log"), {
    write: true,
    create: true,
    truncate: true,
  });

  let child: Deno.ChildProcess;
  try {
    args.portLease?.releaseForSpawn();
    child = new Deno.Command(command.cmd, {
      args: command.args.map((arg) =>
        arg.replaceAll("{config}", args.configPath)
      ),
      cwd: command.cwd,
      env: {
        ...command.env,
        TOKIO_WORKER_THREADS: command.env?.TOKIO_WORKER_THREADS ?? "2",
        TRELLIS_CONFIG: args.configPath,
        NO_COLOR: "1",
      },
      stdin: "null",
      stdout: "piped",
      stderr: "piped",
    }).spawn();
  } catch (error) {
    args.portLease?.release();
    stdoutLog.close();
    stderrLog.close();
    throw error;
  }
  const status = child.status;

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
  ).catch((error) => {
    stdoutTail.append(`\n<failed to read stdout: ${String(error)}>\n`);
  }).finally(() => {
    stdoutLog.close();
  });
  const stderrReader = captureProcessOutput(
    child.stderr,
    stderrTail,
    recordBootstrapLine,
    stderrLog,
  ).catch((error) => {
    stderrTail.append(`\n<failed to read stderr: ${String(error)}>\n`);
  }).finally(() => {
    stderrLog.close();
  });

  handle = new StartedTrellisProcess({
    trellisUrl: args.trellisUrl,
    child,
    status,
    stdoutReader,
    stderrReader,
    stdoutTail,
    stderrTail,
    shutdownTimeoutMs: args.shutdownTimeoutMs,
  });

  try {
    await waitForTrellisReady({
      trellisUrl: args.trellisUrl,
      startupTimeoutMs: args.startupTimeoutMs,
      status,
      stdoutTail,
      stderrTail,
      readers: [stdoutReader, stderrReader],
    });
    args.portLease?.release();
    return handle;
  } catch (error) {
    args.portLease?.release();
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
