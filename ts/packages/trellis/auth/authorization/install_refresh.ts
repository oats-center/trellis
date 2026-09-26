// One internal installation path for authorization-context refresh.
//
// Background refresh and the client/service/device runtimes all used to repeat
// the same prepare/reconnect/retain/promote sequence. Keeping one implementation
// means a planned authorization-credential rotation is classified once, as
// maintenance of the existing logical Trellis connection, and every runtime
// observes the same admission and promotion semantics.

import {
  beginAuthorizationTransportRotation,
  escalateAuthorizationTransportRotation,
  type TrellisConnection,
} from "../../connection.ts";

/**
 * Provider operations one authorization-refresh installation drives.
 *
 * The installation depends on this narrow contract rather than the concrete
 * cache so the rotation decision can be exercised directly. @internal
 */
export type AuthorizationRefreshProvider = {
  /** Wait for the connected registry to be admitted. @internal */
  waitReady(options: { timeoutMs: number }): Promise<void>;
  /** Exact admitted connection generation to retain the candidate on. @internal */
  connectionGeneration(): number;
  /** Retain exact revocation coverage for the candidate digest. @internal */
  retainOwnCandidate(digest: string, generation: number): Promise<void>;
  /** Promote the candidate once its coverage is current. @internal */
  promoteOwnCandidate(digest: string, generation: number): void;
  /** Abandon the planned rotation so retained guards fail closed. @internal */
  abandonRotation(): void;
};

/** Inputs for one internal authorization-refresh installation. @internal */
export type AuthorizationRefreshInstallation = {
  /** Logical connection the refresh maintains. @internal */
  connection: TrellisConnection;
  /** Process-local provider cache that must retain the candidate coverage. @internal */
  provider: AuthorizationRefreshProvider;
  /** Exact digest of the prepared candidate context. @internal */
  contextDigest: string;
  /** Update transport endpoints before any planned rotation. @internal */
  updateTransport?: () => void | Promise<void>;
  /** Force the physical NATS credential rotation. @internal */
  reconnect: () => Promise<void>;
  /** Bounded wait for admission, coverage and promotion. @internal */
  timeoutMs?: number;
};

/**
 * Install one prepared authorization candidate as transparent maintenance.
 *
 * The physical NATS attachment rotates to admit the replacement credential, but
 * the logical connection is preserved: the predecessor stays
 * application-current until the candidate is admitted, its exact revocation
 * coverage is retained on the replacement attachment, and the candidate is
 * promoted. Failure cancels the planned rotation and escalates to the ordinary
 * transport-loss path so a dead physical attachment never stays logically
 * "connected".
 *
 * Installation never suppresses a genuine recovery: a planned rotation is only
 * begun while the logical connection is healthy. If the scheduled refresh fires
 * during an already-real transport outage, the replacement credential is still
 * installed and the reconnect requested, but the resulting physical reconnect
 * stays a real logical transition so the connection returns to connected and
 * Live resumes instead of being captured as maintenance.
 *
 * @internal
 */
export async function installAuthorizationRefresh(
  args: AuthorizationRefreshInstallation,
): Promise<void> {
  const timeoutMs = args.timeoutMs ?? 30_000;
  // A planned rotation means "Trellis is replacing a healthy attachment solely
  // to install new credentials". It must not mean "credentials are being
  // installed while an already-lost attachment recovers". With raw transport
  // diagnostics no longer mutating the logical phase, the phase is the
  // authoritative distinction: connected rotates as maintenance, anything else
  // is ordinary recovery.
  const planned = args.connection.status.phase === "connected";
  const rotation = planned
    ? beginAuthorizationTransportRotation(args.connection)
    : undefined;
  try {
    await args.updateTransport?.();
    await args.reconnect();
    if (rotation) {
      await bounded(rotation.reconnected, timeoutMs);
    }
    await args.provider.waitReady({ timeoutMs });
    const generation = args.provider.connectionGeneration();
    await args.provider.retainOwnCandidate(args.contextDigest, generation);
    args.provider.promoteOwnCandidate(args.contextDigest, generation);
    rotation?.complete();
  } catch (error) {
    args.provider.abandonRotation();
    if (rotation) {
      // Fail closed: the rotation will not be promoted, so retained Live guards
      // must stop waiting and report the real authority loss. Escalating also
      // clears the planned-rotation classification; when a physical loss was
      // suppressed and the expected reconnect never happened, it is published as a
      // genuine logical disconnect rather than left hidden.
      escalateAuthorizationTransportRotation(args.connection);
    }
    throw error;
  }
}

function bounded<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error("planned authorization transport rotation timed out"));
    }, timeoutMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}
