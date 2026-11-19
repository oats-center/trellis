/**
 * A stand-in for systemd's notification socket: a Unix datagram socket bound
 * at a path or an `@` abstract name, collecting each `sd_notify` message a service process sends.
 * Node has no Unix datagram sockets, so it binds one through `node:ffi`.
 */
import { createRequire } from "node:module";

type Libc = {
  socket(domain: number, type: number, protocol: number): number;
  bind(fd: number, address: Buffer, length: number): number;
  recv(fd: number, buffer: Buffer, length: bigint, flags: number): bigint;
  close(fd: number): number;
};

const AF_UNIX = 1;
const SOCK_DGRAM = 2;
const SOCK_NONBLOCK = 0o4000;
const SOCK_CLOEXEC = 0o2000000;

/** One received notification with when it arrived. */
export type Notification = { at: number; fields: Record<string, string> };

/** Listens on `path` until closed, keeping every notification received. */
export class NotifySocket {
  readonly path: string;
  readonly received: Notification[] = [];
  readonly #libc: Libc;
  readonly #fd: number;
  readonly #timer: ReturnType<typeof setInterval>;

  constructor(path: string) {
    const ffi = createRequire(import.meta.url)("node:ffi");
    this.#libc = ffi.dlopen(null, {
      socket: { arguments: ["int32", "int32", "int32"], return: "int32" },
      bind: { arguments: ["int32", "buffer", "uint32"], return: "int32" },
      recv: {
        arguments: ["int32", "buffer", "uint64", "int32"],
        return: "int64",
      },
      close: { arguments: ["int32"], return: "int32" },
    }).functions as Libc;
    this.path = path;
    this.#fd = this.#libc.socket(
      AF_UNIX,
      SOCK_DGRAM | SOCK_NONBLOCK | SOCK_CLOEXEC,
      0,
    );
    // `@name` is Linux's abstract namespace, as systemd allows: no file, and
    // no 108-byte path limit to trip over in a deep test directory.
    const address = Buffer.alloc(110);
    address.writeUInt16LE(AF_UNIX, 0);
    const name = Buffer.from(path.startsWith("@") ? path.slice(1) : path);
    name.copy(address, path.startsWith("@") ? 3 : 2);
    const length = path.startsWith("@") ? 3 + name.length : 110;
    if (this.#fd < 0 || this.#libc.bind(this.#fd, address, length) !== 0)
      throw new Error(`cannot bind a notification socket at ${path}`);
    const buffer = Buffer.alloc(4096);
    this.#timer = setInterval(() => {
      for (;;) {
        const length = Number(this.#libc.recv(this.#fd, buffer, 4096n, 0));
        if (length <= 0) return;
        const fields: Record<string, string> = {};
        for (const line of buffer.subarray(0, length).toString().split("\n")) {
          const split = line.indexOf("=");
          if (split > 0) fields[line.slice(0, split)] = line.slice(split + 1);
        }
        this.received.push({ at: Date.now(), fields });
      }
    }, 10);
  }

  /** The first notification that sets `field`, optionally to `value`. */
  find(field: string, value?: string): Notification | undefined {
    return this.received.find(
      (notification) =>
        field in notification.fields &&
        (value === undefined || notification.fields[field] === value),
    );
  }

  close(): void {
    clearInterval(this.#timer);
    this.#libc.close(this.#fd);
  }
}
