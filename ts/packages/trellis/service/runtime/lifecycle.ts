/**
 * A service process's lifecycle as the outside sees it: its phase, systemd's
 * notification socket, and an optional readiness probe.
 *
 * @module
 */
import { rmSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { createRequire } from "node:module";
import type { LoggerLike } from "../../globals.ts";

/**
 * Where a connected service is in its lifecycle.
 *
 * - `connecting`: connected with its build installed, but not every handler,
 *   job worker and consumer takes work yet; also while it reconnects after
 *   losing its connection.
 * - `serving`: every registered handler, job worker and consumer takes work.
 * - `stopping`: takes no new work and finishes what it holds.
 * - `stopped`: closed.
 */
export type ServicePhase = "connecting" | "serving" | "stopping" | "stopped";

/** Readiness probe for orchestrators that can't use systemd notifications. */
export type ServiceReadinessOpts = {
  /** A file that exists, containing `serving`, only while the service serves. */
  file?: string;
  /**
   * A small HTTP endpoint: `GET /ready` answers 200 while the service serves
   * and 503 otherwise; `GET /live` answers 200 until it has stopped.
   */
  http?: {
    port: number;
    /** Interface to listen on. Every interface when left out. */
    host?: string;
  };
};

type Libc = {
  socket(domain: number, type: number, protocol: number): number;
  sendto(
    fd: number,
    message: Buffer,
    length: bigint,
    flags: number,
    address: Buffer,
    addressLength: number,
  ): bigint;
  close(fd: number): number;
};

type Ffi = {
  dlopen(
    path: null,
    symbols: Record<string, { arguments: string[]; return: string }>,
  ): { functions: unknown };
};

const AF_UNIX = 1;
const SOCK_DGRAM = 2;
const SOCK_CLOEXEC = 0o2000000;
const MSG_NOSIGNAL = 0x4000;

/**
 * Sends `sd_notify` messages to the socket in `NOTIFY_SOCKET`. Node has no
 * Unix datagram sockets, so this calls libc through `node:ffi` (Node 26 and
 * newer); without it the service still runs and says so once.
 */
function systemdNotifier(
  log: LoggerLike,
): ((message: string) => void) | undefined {
  const socket = process.env.NOTIFY_SOCKET;
  if (!socket) return undefined;
  let libc: Libc;
  try {
    // node:ffi announces itself as experimental on first load. The SDK uses
    // three libc calls; that notice is for the service's operator to ignore.
    const emitWarning = process.emitWarning;
    process.emitWarning = ((warning: string | Error, ...rest: never[]) => {
      const message = typeof warning === "string" ? warning : warning.message;
      if (message.includes("FFI")) return;
      return emitWarning.call(process, warning, ...rest);
    }) as typeof process.emitWarning;
    let ffi: Ffi;
    try {
      ffi = createRequire(import.meta.url)("node:ffi");
    } finally {
      process.emitWarning = emitWarning;
    }
    libc = ffi.dlopen(null, {
      socket: { arguments: ["int32", "int32", "int32"], return: "int32" },
      sendto: {
        arguments: ["int32", "buffer", "uint64", "int32", "buffer", "uint32"],
        return: "int64",
      },
      close: { arguments: ["int32"], return: "int32" },
    }).functions as Libc;
  } catch (error) {
    log.warn(
      { error, notifySocket: socket },
      "NOTIFY_SOCKET is set, but sending to it needs node:ffi (Node 26 or newer); systemd will not hear when this service serves",
    );
    return undefined;
  }
  // `@name` is the abstract namespace; its address starts with a NUL byte.
  const abstract = socket.startsWith("@");
  const name = Buffer.from(abstract ? socket.slice(1) : socket);
  const address = Buffer.alloc(2 + 108);
  address.writeUInt16LE(AF_UNIX, 0);
  name.copy(address, abstract ? 3 : 2);
  const addressLength = abstract ? 3 + name.length : address.length;
  const fd = libc.socket(AF_UNIX, SOCK_DGRAM | SOCK_CLOEXEC, 0);
  let warned = false;
  return (message) => {
    const body = Buffer.from(message);
    const sent =
      fd >= 0 &&
      libc.sendto(
        fd,
        body,
        BigInt(body.length),
        MSG_NOSIGNAL,
        address,
        addressLength,
      ) === BigInt(body.length);
    if (!sent && !warned) {
      warned = true;
      log.warn(
        { notifySocket: socket },
        "Could not send a notification to NOTIFY_SOCKET",
      );
    }
  };
}

/**
 * Owns one service process's phase and tells systemd and the readiness probe
 * about it. @internal
 */
export class ServiceLifecycle {
  #phase: ServicePhase = "connecting";
  #served = false;
  readonly #ready = Promise.withResolvers<void>();
  readonly #listeners = new Set<(phase: ServicePhase) => void>();
  readonly #notify?: (message: string) => void;
  readonly #file?: string;
  readonly #server?: Server;
  readonly #log: LoggerLike;

  constructor(readiness: ServiceReadinessOpts | undefined, log: LoggerLike) {
    this.#log = log;
    this.#notify = systemdNotifier(log);
    this.#file = readiness?.file;
    if (this.#file) rmSync(this.#file, { force: true });
    const http = readiness?.http;
    if (http) {
      const server = createServer((request, response) => {
        const path = request.url?.split("?")[0];
        const [status, body] =
          path === "/ready"
            ? this.#phase === "serving"
              ? [200, "ready"]
              : [503, this.#phase]
            : path === "/live"
              ? this.#phase === "stopped"
                ? [503, "stopped"]
                : [200, "live"]
              : [404, "not found"];
        response.writeHead(status, { "content-type": "text/plain" });
        response.end(`${body}\n`);
      });
      server.on("error", (error) =>
        log.warn({ error, port: http.port }, "Readiness probe stopped"),
      );
      server.listen(http.port, http.host);
      // The probe reports on the process; it never keeps it running.
      server.unref();
      this.#server = server;
    }
    this.status("Connecting to Trellis");
  }

  /** The current phase. */
  get phase(): ServicePhase {
    return this.#phase;
  }

  /** Resolves the first time the service serves. */
  get ready(): Promise<void> {
    return this.#ready.promise;
  }

  /** Calls `listener` on every later phase change; returns its removal. */
  subscribe(listener: (phase: ServicePhase) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  /** Tells systemd what the service is doing, without changing its phase. */
  status(text: string): void {
    this.#notify?.(`STATUS=${text}`);
  }

  /** Moves to `phase`; leaving `stopping` or `stopped` is not possible. */
  set(phase: ServicePhase): void {
    if (phase === this.#phase) return;
    if (this.#phase === "stopped") return;
    if (this.#phase === "stopping" && phase !== "stopped") return;
    this.#phase = phase;
    if (phase === "serving") {
      this.#notify?.(
        this.#served ? "STATUS=Serving" : "READY=1\nSTATUS=Serving",
      );
      this.#served = true;
      this.#ready.resolve();
    } else if (phase === "connecting") {
      this.status("Reconnecting to Trellis");
    } else if (phase === "stopping") {
      this.#notify?.("STOPPING=1\nSTATUS=Stopping");
    }
    try {
      if (this.#file && phase === "serving")
        writeFileSync(this.#file, "serving\n");
      else if (this.#file) rmSync(this.#file, { force: true });
    } catch (error) {
      this.#log.warn(
        { error, file: this.#file },
        "Could not update the readiness file",
      );
    }
    if (phase === "stopped") this.#server?.close();
    for (const listener of [...this.#listeners]) listener(phase);
  }
}
