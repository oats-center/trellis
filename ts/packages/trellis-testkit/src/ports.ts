import { randomInt } from "node:crypto";
import { createSocket } from "node:dgram";
import { readFile } from "node:fs/promises";
import { createServer, type Server } from "node:net";
import { z } from "zod";

// Keep automatic HTTP/WSS listeners above Fetch's restricted low ports (through 10080).
const MIN_AUTOMATIC_PORT = 10_240;

/**
 * Owned port reservation. Its owner keeps it for as long as the port is
 * theirs, across every child process it hands the port to.
 */
export type ReservedPort = {
  /** Selected TCP port. */
  readonly port: number;
  /** Closes the TCP listener immediately before a child process binds the port. */
  releaseForSpawn(): Promise<void>;
  /**
   * Listens on the port again once the child it was handed to has let go of
   * it, so the next child can be handed it the same way. Rejects with
   * `EADDRINUSE` while anything still listens there.
   */
  reacquire(): Promise<void>;
  /** Releases all ownership, once no child will be handed the port again. */
  release(): Promise<void>;
};

/** Listens on loopback TCP `port`. */
async function listenOn(port: number): Promise<Server> {
  const listener = createServer();
  await new Promise<void>((resolve, reject) => {
    listener.once("error", reject);
    listener.listen(port, "127.0.0.1", () => {
      listener.removeListener("error", reject);
      resolve();
    });
  });
  return listener;
}

/**
 * Reserves a loopback TCP port, chosen automatically or the given one.
 * Automatic choices exclude Linux's outbound ephemeral range so bootstrap
 * connections cannot steal a reservation after its release for process spawn.
 * A UDP socket on the same port, held until {@link ReservedPort.release},
 * keeps other testkit processes out of the port without preventing the
 * production child's TCP bind: while the child starts, and while a restarted
 * child is down and has let go of it.
 */
export async function reserveLocalPort(port = 0): Promise<ReservedPort> {
  let lowerCount = 0;
  let upperStart = 0;
  let availableCount = 0;
  if (port === 0) {
    const bound = z.coerce.number().int().min(1).max(65535);
    const [first, last] = z
      .tuple([bound, bound])
      .refine(([first, last]) => first <= last)
      .parse(
        (await readFile("/proc/sys/net/ipv4/ip_local_port_range", "utf8"))
          .trim()
          .split(/\s+/),
      );
    lowerCount = Math.max(0, first - MIN_AUTOMATIC_PORT);
    upperStart = Math.max(MIN_AUTOMATIC_PORT, last + 1);
    availableCount = lowerCount + 65536 - upperStart;
    if (availableCount === 0)
      throw new Error(
        "No nonprivileged listener ports outside the kernel ephemeral range",
      );
  }
  for (let attempt = 0; attempt < 32; attempt++) {
    const offset = port === 0 ? randomInt(availableCount) : 0;
    const selected =
      port !== 0
        ? port
        : offset < lowerCount
          ? MIN_AUTOMATIC_PORT + offset
          : upperStart + offset - lowerCount;
    const ownership = createSocket("udp4");
    let listener: Server;
    try {
      await new Promise<void>((resolve, reject) => {
        ownership.once("error", reject);
        ownership.bind(selected, "127.0.0.1", () => {
          ownership.removeListener("error", reject);
          resolve();
        });
      });
      listener = await listenOn(selected);
    } catch (error) {
      await new Promise<void>((resolve) => ownership.close(resolve));
      if (
        port === 0 &&
        error instanceof Error &&
        "code" in error &&
        error.code === "EADDRINUSE"
      )
        continue;
      throw error;
    }
    const address = listener.address();
    if (!address || typeof address === "string")
      throw new Error("TCP reservation has no address");
    let closing: Promise<void> | undefined;
    const releaseForSpawn = () => {
      closing ??= new Promise<void>((resolve, reject) => {
        listener.close((error) => (error ? reject(error) : resolve()));
      });
      return closing;
    };
    const reacquire = async () => {
      await releaseForSpawn();
      listener = await listenOn(selected);
      closing = undefined;
    };
    let releasingOwnership: Promise<void> | undefined;
    const release = async () => {
      await releaseForSpawn();
      releasingOwnership ??= new Promise<void>((resolve) =>
        ownership.close(resolve),
      );
      await releasingOwnership;
    };
    return { port: address.port, releaseForSpawn, reacquire, release };
  }
  throw new Error("Unable to reserve an available non-ephemeral listener port");
}
