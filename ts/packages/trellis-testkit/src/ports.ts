import { createServer } from "node:net";

/** Owned TCP reservation, held until the production child is ready to bind. */
export type ReservedPort = {
  /** Selected TCP port. */
  readonly port: number;
  /** Releases the listener immediately before process spawn. */
  releaseForSpawn(): Promise<void>;
  /** Releases the listener during cleanup. */
  release(): Promise<void>;
};

/** Reserves a loopback TCP port, optionally reacquiring a restart's prior port. */
export async function reserveLocalPort(port = 0): Promise<ReservedPort> {
  const listener = createServer();
  await new Promise<void>((resolve, reject) => {
    listener.once("error", reject);
    listener.listen(port, "127.0.0.1", () => {
      listener.removeListener("error", reject);
      resolve();
    });
  });
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
