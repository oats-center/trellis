// One internal installation path for authorization-context refresh.
//
// Routine renewal is not a transport operation. A refreshed context that is
// covered by the current physical attachment is validated, retained, and
// promoted in place: the socket is never replaced, no sequence/credit state is
// reset, and no Live session is recreated. If no candidate-safe carrier remains,
// coverage warms privately on the same socket ordinary adoption will activate.

import type { VerifiedAuthorizationContext } from "./types.ts";
import type { AuthorizationContextCache } from "./client_context.ts";
import type {
  TransportGenerationPrepared,
  TransportOwnCoveragePreparation,
} from "../../transport/generations.ts";

/** Immutable original candidate and CONNECT companions. @internal */
export type AuthorizationCandidateSnapshot = ReturnType<
  AuthorizationContextCache["candidateSnapshot"]
>;

/** Exact preparation and pin owner for one installation. @internal */
export type AuthorizationRefreshAttempt = Readonly<{
  origin: VerifiedAuthorizationContext;
  cacheEpoch: number;
}>;

/**
 * Provider operations one authorization-refresh installation drives.
 *
 * The installation depends on this narrow contract rather than the concrete
 * cache. @internal
 */
export type AuthorizationRefreshProvider = {
  /** Cache epoch (verifier stop/reset) the candidate is retained under. @internal */
  cacheEpoch(): number;
  /** Retain safe existing coverage for this exact preparation and attempt. @internal */
  retainOwnCandidate(
    attempt: AuthorizationRefreshAttempt,
    prepareConnect?: (
      snapshot: AuthorizationCandidateSnapshot,
    ) => Promise<TransportGenerationPrepared>,
  ): Promise<TransportOwnCoveragePreparation | undefined>;
  /** Promote the candidate once its coverage is current. @internal */
  promoteOwnCandidate(attempt: AuthorizationRefreshAttempt): void;
  /** Drop an unadmitted prepared candidate after a failed renewal. @internal */
  releaseCandidate(attempt: AuthorizationRefreshAttempt): void;
};

/** Inputs for one internal authorization-refresh installation. @internal */
export type AuthorizationRefreshInstallation = {
  /** Process-local provider cache that must retain the candidate coverage. @internal */
  provider: AuthorizationRefreshProvider;
  /** Original verified object returned by this refresh's prepare. @internal */
  origin: VerifiedAuthorizationContext;
  /** Prepare CONNECT from the original candidate, only when no carrier remains. @internal */
  prepareConnect?: (
    snapshot: AuthorizationCandidateSnapshot,
  ) => Promise<TransportGenerationPrepared>;
  /** Synchronous successful tail before any projection await. @internal */
  onPromoted?: () => void;
};

/**
 * Install a candidate with exact retained revocation coverage. Managed renewal
 * borrows one safe admitted carrier or privately prepares one exact candidate
 * socket when none remains. The synchronous finish fence promotes authority and
 * parks that same socket before ordinary adoption is notified. Failed warming
 * cancels only this preparation and leaves installed authority untouched. A
 * fixed-socket cache retains its existing installation path.
 *
 * @internal
 */
export async function installAuthorizationRefresh(
  args: AuthorizationRefreshInstallation,
): Promise<void> {
  const attempt: AuthorizationRefreshAttempt = Object.freeze({
    origin: args.origin,
    cacheEpoch: args.provider.cacheEpoch(),
  });
  let preparation: TransportOwnCoveragePreparation | undefined;
  try {
    preparation = await args.provider.retainOwnCandidate(
      attempt,
      args.prepareConnect,
    );
    if (preparation) {
      if (
        !preparation.finish(() => {
          args.provider.promoteOwnCandidate(attempt);
          return true;
        })
      ) {
        throw new Error(
          "authorization candidate coverage changed before promotion",
        );
      }
    } else args.provider.promoteOwnCandidate(attempt);
    args.onPromoted?.();
  } catch (error) {
    args.provider.releaseCandidate(attempt);
    await preparation?.cancel();
    throw error;
  }
}
