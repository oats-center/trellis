/**
 * Manages an isolated local nats-server process for Trellis tests.
 *
 * `NatsTestContainer.start` generates an account bootstrap with the pinned
 * `nsc` binary, spawns the pinned `nats-server` binary as a child process
 * with stdout/stderr captured to log files under the workdir, and tracks the
 * child in a pid file so crashed test runs can be cleaned up later.
 */
import type { NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import { join } from "@std/path";

import {
  formatPidFile,
  parsePidFile,
  type ProcessIdentity,
  processIsGone,
  processMatchesIdentity,
  recordProcessIdentity,
  removeStalePidNamedResources,
} from "./cleanup.ts";
import { reserveLocalPort } from "./control_plane_config.ts";
import { ensureNatsBinaries } from "./nats_binaries.ts";
import {
  generateLocalNatsBootstrap,
  type LocalNatsBootstrapManifest,
  type NatsBootstrapPorts,
} from "./nats_bootstrap.ts";
import {
  captureProcessOutput,
  type CommandStatus,
  commandStatusText,
  killProcess,
  settledStatus,
  TextTail,
  waitForStatus,
} from "./trellis_process.ts";

const NATS_PID_FILE_PREFIX = "nats-";
const DEFAULT_STARTUP_MS = 30_000;
const SHUTDOWN_GRACE_MS = 10_000;
const OUTPUT_TAIL_CHARS = 8_192;

type StartedNatsContainer = {
  natsUrl: string;
  websocketUrl: string;
  manifest: LocalNatsBootstrapManifest;
  nc: NatsConnection;
  child?: Deno.ChildProcess;
  status?: Promise<CommandStatus>;
  pidFile?: string;
  pidFileContent?: string;
  readers?: readonly Promise<void>[];
};

type StartNatsTestContainerOptions = {
  startupMs?: number;
};

async function readRegularFileContent(
  path: string,
): Promise<string | undefined> {
  // Require a regular file before any read: a symlink cannot be trusted and a
  // FIFO must never be opened (a read on a FIFO with no writer blocks).
  try {
    const stat = await Deno.lstat(path);
    if (!stat.isFile) return undefined;
  } catch {
    return undefined;
  }
  return await Deno.readTextFile(path).catch(() => undefined);
}

/** Best-effort SIGTERM then SIGKILL for a pid left by a crashed test run. */
async function killPidGracefully(identity: ProcessIdentity): Promise<void> {
  if (
    !await processIsGone(identity.pid) &&
    await processMatchesIdentity(identity)
  ) {
    try {
      Deno.kill(identity.pid, "SIGTERM");
    } catch {
      return; // already exited
    }
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  if (await processIsGone(identity.pid)) return;
  // Recheck the identity immediately before SIGKILL: the pid may have been
  // recycled during the SIGTERM grace period.
  if (!await processMatchesIdentity(identity)) return;
  try {
    Deno.kill(identity.pid, "SIGKILL");
  } catch {
    // already exited during the grace period
  }
}

/** @internal Removes one stale nats pid file, killing its child only when the identity still matches. */
export async function cleanupStaleNatsPidFile(path: string): Promise<void> {
  // Snapshot the content before the grace wait: a pid file replaced by
  // another owner during the wait must never be unlinked.
  const snapshot = await readRegularFileContent(path);
  if (snapshot === undefined) return; // missing or symlinked: never touched
  const identity = parsePidFile(snapshot);
  if (identity !== undefined) await killPidGracefully(identity);
  // Unlink only when the content is unchanged and still a regular file.
  if ((await readRegularFileContent(path)) === snapshot) {
    await Deno.remove(path).catch(() => undefined);
  }
}

async function removeStaleNatsPidFiles(natsDir: string): Promise<void> {
  const names: string[] = [];
  try {
    for await (const entry of Deno.readDir(natsDir)) names.push(entry.name);
  } catch {
    return;
  }
  await removeStalePidNamedResources({
    names,
    prefix: NATS_PID_FILE_PREFIX,
    remove: async (name) => {
      await cleanupStaleNatsPidFile(join(natsDir, name));
    },
  });
}

async function portIsOpen(port: number): Promise<boolean> {
  try {
    const conn = await Deno.connect({ hostname: "127.0.0.1", port });
    conn.close();
    return true;
  } catch {
    return false;
  }
}

async function waitForNatsPorts(args: {
  ports: readonly number[];
  startupMs: number;
  status: Promise<CommandStatus>;
  output(): string;
  readers: readonly Promise<void>[];
}): Promise<void> {
  const startedAt = Date.now();
  while (Date.now() - startedAt <= args.startupMs) {
    const status = await settledStatus(args.status);
    if (status !== undefined) {
      await Promise.allSettled(args.readers);
      throw new Error(
        `nats-server exited before readiness (${
          commandStatusText(status)
        })\n${args.output()}`,
      );
    }
    const open = await Promise.all(args.ports.map((port) => portIsOpen(port)));
    if (open.every(Boolean)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(
    `timed out after ${args.startupMs}ms waiting for nats-server on ${
      args.ports.map((port) => `127.0.0.1:${port}`).join(", ")
    }\n${args.output()}`,
  );
}

/** @internal Removes the pid file only while it still contains this run's written content. */
export async function removeOwnedPidFile(
  path: string,
  writtenContent: string,
): Promise<void> {
  // A missing, symlinked, or non-regular pid file (e.g. a FIFO) is not ours to
  // touch; a FIFO must never be opened.
  const stat = await Deno.lstat(path).catch(() => undefined);
  if (stat === undefined || !stat.isFile) return;
  const current = await Deno.readTextFile(path).catch(() => undefined);
  if (current !== writtenContent) return; // another owner replaced it
  await Deno.remove(path).catch(() => undefined);
}

async function stopNatsChild(args: {
  child: Deno.ChildProcess;
  status: Promise<CommandStatus>;
  pidFile: string;
  pidFileContent: string | undefined;
  readers: readonly Promise<void>[];
}): Promise<void> {
  // Remove the pid file only while it still contains the content this run
  // wrote; if another owner replaced it, leave their file alone.
  if (args.pidFileContent !== undefined) {
    await removeOwnedPidFile(args.pidFile, args.pidFileContent);
  }
  const alreadyExited = await settledStatus(args.status);
  if (alreadyExited === undefined) {
    killProcess(args.child, "SIGTERM");
    const terminated = await waitForStatus(args.status, SHUTDOWN_GRACE_MS);
    if (terminated === undefined) {
      killProcess(args.child, "SIGKILL");
      await args.status.catch(() => undefined);
    }
  }
  await Promise.allSettled(args.readers);
}

/** Manages an isolated local NATS/JetStream server for Trellis tests. */
export class NatsTestContainer implements AsyncDisposable {
  readonly natsUrl: string;
  readonly websocketUrl: string;
  readonly manifest: LocalNatsBootstrapManifest;
  readonly nc: NatsConnection;
  readonly #child: Deno.ChildProcess | undefined;
  readonly #status: Promise<CommandStatus> | undefined;
  readonly #pidFile: string | undefined;
  readonly #pidFileContent: string | undefined;
  readonly #readers: readonly Promise<void>[];
  #stopped = false;

  private constructor(started: StartedNatsContainer) {
    this.natsUrl = started.natsUrl;
    this.websocketUrl = started.websocketUrl;
    this.manifest = started.manifest;
    this.nc = started.nc;
    this.#child = started.child;
    this.#status = started.status;
    this.#pidFile = started.pidFile;
    this.#pidFileContent = started.pidFileContent;
    this.#readers = started.readers ?? [];
  }

  /** Starts a fresh local nats-server under `workdir`. */
  static async start(
    workdir: string,
    options: StartNatsTestContainerOptions = {},
  ): Promise<NatsTestContainer> {
    for (let attempt = 1;; attempt++) {
      try {
        return await NatsTestContainer.startOnce(workdir, options);
      } catch (error) {
        if (
          attempt >= 3 ||
          !String(error).toLowerCase().includes("address already in use")
        ) {
          throw error;
        }
      }
    }
  }

  private static async startOnce(
    workdir: string,
    options: StartNatsTestContainerOptions,
  ): Promise<NatsTestContainer> {
    const natsDir = join(workdir, "nats");
    await Deno.mkdir(natsDir, { recursive: true });
    await removeStaleNatsPidFiles(natsDir);
    const binaries = await ensureNatsBinaries();
    const portLeases = {
      nats: reserveLocalPort(),
      http: reserveLocalPort(),
      websocket: reserveLocalPort(),
    };
    try {
      const ports: NatsBootstrapPorts = {
        nats: portLeases.nats.port,
        http: portLeases.http.port,
        websocket: portLeases.websocket.port,
      };
      const manifest = await generateLocalNatsBootstrap({
        outDir: natsDir,
        ports,
      });
      const stdoutLog = await Deno.open(join(natsDir, "nats.stdout.log"), {
        write: true,
        create: true,
        truncate: true,
      });
      const stderrLog = await Deno.open(join(natsDir, "nats.stderr.log"), {
        write: true,
        create: true,
        truncate: true,
      });
      const stdoutTail = new TextTail(OUTPUT_TAIL_CHARS);
      const stderrTail = new TextTail(OUTPUT_TAIL_CHARS);
      let child: Deno.ChildProcess;
      try {
        for (const lease of Object.values(portLeases)) lease.releaseForSpawn();
        child = new Deno.Command(binaries.natsServer, {
          args: ["-c", join(natsDir, manifest.paths.natsConfig)],
          cwd: natsDir,
          stdin: "null",
          stdout: "piped",
          stderr: "piped",
        }).spawn();
      } catch (error) {
        for (const lease of Object.values(portLeases)) lease.release();
        try {
          stdoutLog.close();
          stderrLog.close();
        } catch {
          // best-effort fd cleanup on a failed spawn
        }
        throw error;
      }
      const status = child.status;
      const pidFile = join(
        natsDir,
        `${NATS_PID_FILE_PREFIX}${Deno.pid}-${Date.now()}.pid`,
      );
      const readers: Promise<void>[] = [];
      let pidFileContent: string | undefined;
      let nc: NatsConnection | undefined;

      try {
        const identity = await recordProcessIdentity(
          child.pid,
          binaries.natsServer,
        );
        // Exclusive create: a leftover pid file from a live run must fail loudly
        // instead of being clobbered.
        const content = formatPidFile(identity);
        await Deno.writeTextFile(pidFile, content, { createNew: true });
        pidFileContent = content;
        const stdoutReader = captureProcessOutput(
          child.stdout,
          stdoutTail,
          () => undefined,
          stdoutLog,
        ).catch((error) => {
          stdoutTail.append(`\n<failed to read stdout: ${String(error)}>\n`);
        }).finally(() => {
          stdoutLog.close();
        });
        const stderrReader = captureProcessOutput(
          child.stderr,
          stderrTail,
          () => undefined,
          stderrLog,
        ).catch((error) => {
          stderrTail.append(`\n<failed to read stderr: ${String(error)}>\n`);
        }).finally(() => {
          stderrLog.close();
        });
        readers.push(stdoutReader, stderrReader);
        const output = () => {
          const stdout = stdoutTail.toString().trimEnd() || "<empty>";
          const stderr = stderrTail.toString().trimEnd() || "<empty>";
          return `stdout tail:\n${stdout}\nstderr tail:\n${stderr}`;
        };

        const startupMs = options.startupMs ?? DEFAULT_STARTUP_MS;
        await waitForNatsPorts({
          ports: [ports.nats, ports.http, ports.websocket],
          startupMs,
          status,
          output,
          readers,
        });
        for (const lease of Object.values(portLeases)) lease.release();
        const natsUrl = `nats://127.0.0.1:${ports.nats}`;
        const websocketUrl = `ws://127.0.0.1:${ports.websocket}`;
        nc = await connect({
          servers: natsUrl,
          authenticator: credsAuthenticator(
            await Deno.readFile(
              join(natsDir, manifest.paths.creds.trellisService),
            ),
          ),
        });
        return new NatsTestContainer({
          natsUrl,
          websocketUrl,
          manifest,
          nc,
          child,
          status,
          pidFile,
          pidFileContent,
          readers,
        });
      } catch (error) {
        for (const lease of Object.values(portLeases)) lease.release();
        if (nc && !nc.isClosed()) await nc.close().catch(() => undefined);
        await stopNatsChild({
          child,
          status,
          pidFile,
          pidFileContent,
          readers,
        });
        throw error;
      }
    } finally {
      for (const lease of Object.values(portLeases)) lease.release();
    }
  }

  /** Stops the NATS connection and the managed nats-server process. */
  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    let closeError: unknown;
    if (!this.nc.isClosed()) {
      try {
        await this.nc.close();
      } catch (error) {
        closeError = error;
      }
    }
    if (
      this.#child !== undefined && this.#status !== undefined &&
      this.#pidFile !== undefined
    ) {
      await stopNatsChild({
        child: this.#child,
        status: this.#status,
        pidFile: this.#pidFile,
        pidFileContent: this.#pidFileContent,
        readers: this.#readers,
      });
    }
    if (closeError) throw closeError;
  }

  [Symbol.asyncDispose](): Promise<void> {
    return this.stop();
  }
}
