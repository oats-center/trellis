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
import type { AuthorizationProviderCache } from "./provider_cache.ts";

/** Inputs for one internal authorization-refresh installation. @internal */
export type AuthorizationRefreshInstallation = {
  /** Logical connection the refresh maintains. @internal */
  connection: TrellisConnection;
  /** Process-local provider cache that must retain the candidate coverage. @internal */
  provider: AuthorizationProviderCache;
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
 * The physical NATS attachment may rotate, but the logical connection is
 * preserved: the predecessor stays application-current until the candidate is
 * admitted, its exact revocation coverage is retained on the replacement
 * attachment, and the candidate is promoted. Failure cancels the planned
 * rotation and escalates to the ordinary transport-loss path so a dead
 * physical attachment never stays logically "connected".
 *
 * @internal
 */
export async function installAuthorizationRefresh(
  args: AuthorizationRefreshInstallation,
): Promise<void> {
  const timeoutMs = args.timeoutMs ?? 30_000;
  // A planned rotation is only meaningful on a logically connected attachment;
  // otherwise this is ordinary reconciliation on the current attachment.
  const rotates = args.connection.status.phase === "connected";
  const rotation = rotates
    ? beginAuthorizationTransportRotation(args.connection)
    : undefined;
  try {
    await args.updateTransport?.();
    if (rotation) {
      await args.reconnect();
      await bounded(rotation.reconnected, timeoutMs);
    }
    await args.provider.waitReady({ timeoutMs });
    const generation = args.provider.connectionGeneration();
    await args.provider.retainOwnCandidate(args.contextDigest, generation);
    args.provider.promoteOwnCandidate(args.contextDigest, generation);
    rotation?.complete();
  } catch (error) {
    if (rotation) {
      // Fail closed: the rotation will not be promoted, so retained Live
      // guards must stop waiting and report the real authority loss. Escalating
      // also clears the planned-rotation classification; when a physical loss
      // was suppressed and the expected reconnect never happened, it is
      // published as a genuine logical disconnect rather than left hidden.
      args.provider.abandonRotation();
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
