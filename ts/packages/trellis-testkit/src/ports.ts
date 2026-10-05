/** Owned TCP reservation, held until the production child is ready to bind. */
export type ReservedPort = {
  /** Selected TCP port. */
  readonly port: number;
  /** Releases the listener immediately before process spawn. */
  releaseForSpawn(): void;
  /** Releases the listener during cleanup. */
  release(): void;
};

/** Reserves a loopback TCP port, optionally reacquiring a restart's prior port. */
export function reserveLocalPort(port = 0): ReservedPort {
  const listener = Deno.listen({ hostname: "127.0.0.1", port });
  let held = true;
  const release = () => {
    if (!held) return;
    held = false;
    listener.close();
  };
  return { port: listener.addr.port, releaseForSpawn: release, release };
}
