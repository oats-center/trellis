import { randomInt } from "node:crypto";
import { createSocket } from "node:dgram";
import { readFile } from "node:fs/promises";
import { createServer } from "node:net";
import { z } from "zod";

// Keep automatic HTTP/WSS listeners above Fetch's restricted low ports (through 10080).
const MIN_AUTOMATIC_PORT = 10_240;

/** Owned port reservation, retained across the production child's TCP handoff. */
export type ReservedPort = {
  /** Selected TCP port. */
  readonly port: number;
  /** Releases the listener immediately before process spawn. */
  releaseForSpawn(): Promise<void>;
  /** Releases all ownership after readiness or during cleanup. */
  release(): Promise<void>;
};

/**
 * Reserves a loopback TCP port, optionally reacquiring a restart's prior port.
 * Automatic choices exclude Linux's outbound ephemeral range so bootstrap
 * connections cannot steal a reservation after its release for process spawn.
 * A UDP socket on the same port keeps other testkit processes out of that
 * release-to-bind gap without preventing the production child's TCP bind.
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
    const listener = createServer();
    const ownership = createSocket("udp4");
    try {
      await new Promise<void>((resolve, reject) => {
        ownership.once("error", reject);
        ownership.bind(selected, "127.0.0.1", () => {
          ownership.removeListener("error", reject);
          resolve();
        });
      });
      await new Promise<void>((resolve, reject) => {
        listener.once("error", reject);
        listener.listen(selected, "127.0.0.1", () => {
          listener.removeListener("error", reject);
          resolve();
        });
      });
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
    let releasingOwnership: Promise<void> | undefined;
    const release = async () => {
      await releaseForSpawn();
      releasingOwnership ??= new Promise<void>((resolve) =>
        ownership.close(resolve),
      );
      await releasingOwnership;
    };
    return { port: address.port, releaseForSpawn, release };
  }
  throw new Error("Unable to reserve an available non-ephemeral listener port");
}
