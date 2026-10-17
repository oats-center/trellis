import { randomInt } from "node:crypto";
import { readFile } from "node:fs/promises";
import { createServer } from "node:net";
import { z } from "zod";

/** Owned TCP reservation, held until the production child is ready to bind. */
export type ReservedPort = {
  /** Selected TCP port. */
  readonly port: number;
  /** Releases the listener immediately before process spawn. */
  releaseForSpawn(): Promise<void>;
  /** Releases the listener during cleanup. */
  release(): Promise<void>;
};

/**
 * Reserves a loopback TCP port, optionally reacquiring a restart's prior port.
 * Automatic choices exclude Linux's outbound ephemeral range so bootstrap
 * connections cannot steal a reservation after its release for process spawn.
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
    lowerCount = Math.max(0, first - 1024);
    upperStart = Math.max(1024, last + 1);
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
          ? 1024 + offset
          : upperStart + offset - lowerCount;
    const listener = createServer();
    try {
      await new Promise<void>((resolve, reject) => {
        listener.once("error", reject);
        listener.listen(selected, "127.0.0.1", () => {
          listener.removeListener("error", reject);
          resolve();
        });
      });
    } catch (error) {
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
    const release = () => {
      closing ??= new Promise<void>((resolve, reject) => {
        listener.close((error) => (error ? reject(error) : resolve()));
      });
      return closing;
    };
    return { port: address.port, releaseForSpawn: release, release };
  }
  throw new Error("Unable to reserve an available non-ephemeral listener port");
}
