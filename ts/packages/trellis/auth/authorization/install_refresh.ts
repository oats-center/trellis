// One internal installation path for authorization-context refresh.
//
// Routine renewal is not a transport operation. A refreshed context that is
// covered by the current physical attachment is validated, retained, and
// promoted in place: the socket is never replaced, no sequence/credit state is
// reset, and no Live session is recreated. Only an explicit transport refresh
// (or a real loss) creates a new attachment.

/**
 * Provider operations one authorization-refresh installation drives.
 *
 * The installation depends on this narrow contract rather than the concrete
 * cache so in-place promotion can be exercised directly. @internal
 */
export type AuthorizationRefreshProvider = {
  /** Wait for the connected registry to be admitted. @internal */
  waitReady(options: { timeoutMs: number }): Promise<void>;
  /** Exact admitted connection generation the candidate is retained on. @internal */
  connectionGeneration(): number;
  /** Retain exact revocation coverage for the candidate digest. @internal */
  retainOwnCandidate(digest: string, generation: number): Promise<void>;
  /** Promote the candidate once its coverage is current. @internal */
  promoteOwnCandidate(digest: string, generation: number): void;
  /** Drop an unadmitted prepared candidate after a failed renewal. @internal */
  releaseCandidate(): void;
};

/** Inputs for one internal authorization-refresh installation. @internal */
export type AuthorizationRefreshInstallation = {
  /** Process-local provider cache that must retain the candidate coverage. @internal */
  provider: AuthorizationRefreshProvider;
  /** Exact digest of the prepared candidate context. @internal */
  contextDigest: string;
  /** Update the retained next-connect endpoint pool before promotion. @internal */
  updateTransport?: () => void | Promise<void>;
  /** Bounded wait for coverage and promotion. @internal */
  timeoutMs?: number;
};

/**
 * Install one prepared authorization candidate as in-place renewal.
 *
 * The candidate is verified and retained against the exact current physical
 * generation, then promoted under the existing installation fence. The physical
 * NATS attachment is untouched, so the socket's admitted policy `A` is
 * unchanged and every active lease, sequence, and Live session continues. If
 * the candidate's coverage cannot be established on the current generation, the
 * predecessor stays application-current and the candidate is dropped.
 *
 * @internal
 */
export async function installAuthorizationRefresh(
  args: AuthorizationRefreshInstallation,
): Promise<void> {
  const timeoutMs = args.timeoutMs ?? 30_000;
  try {
    await args.updateTransport?.();
    await args.provider.waitReady({ timeoutMs });
    const generation = args.provider.connectionGeneration();
    await args.provider.retainOwnCandidate(args.contextDigest, generation);
    args.provider.promoteOwnCandidate(args.contextDigest, generation);
  } catch (error) {
    args.provider.releaseCandidate();
    throw error;
  }
}
