import type { Authenticator } from "@nats-io/nats-core";
import {
  AuthorizationContextCache,
  createAuth,
  refreshAuthorizationContextWithMetadata,
} from "@oatscenter/trellis/auth";
import type {
  TrellisTestClientKey,
  TrellisTestRuntime,
} from "@oatscenter/trellis-test";

/** Ordinary issued NATS material for a registered client. */
export type ClientSession = {
  /** Signer whose session key is bound into the caller's admitted authority. */
  auth: Awaited<ReturnType<typeof createAuth>>;
  /** Normal issued NATS authenticator for the caller's connection. */
  authenticator: Authenticator | Authenticator[];
  /** Current admitted authorization-context digest. */
  contextDigest: string;
  /** Current login session identity. */
  sessionId: string;
  /** Exact authenticated inbox prefix accepted for replies. */
  inboxPrefix: string;
};

/**
 * Reconstructs a registered client's normal issued NATS material from its
 * admitted connection presence and the production context-refresh endpoint.
 * This reads only public admin state and never exposes facade transport state.
 */
export async function clientSession(
  runtime: TrellisTestRuntime,
  key: TrellisTestClientKey,
): Promise<ClientSession> {
  let contextDigest = "";
  const auth = await createAuth({
    sessionKeySeed: key.seed,
    contextDigest: () => contextDigest,
  });
  const connection = await runtime.waitFor(async () => {
    const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
      items: {
        participantId: string;
        connectedAt: bigint;
        loginSessionId?: string | null;
        runtimeConnectionId: string;
        contextDigest: string;
      }[];
    };
    // The caller is the most recently admitted connection for its participant;
    // the test admin's own session was admitted earlier.
    return page.items
      .filter((item) =>
        item.participantId === key.participantId && item.loginSessionId
      )
      .sort((a, b) => Number(b.connectedAt - a.connectedAt))[0];
  });
  if (!connection.loginSessionId) {
    throw new Error("test client has no admitted login session");
  }
  contextDigest = connection.contextDigest;
  const { response } = await refreshAuthorizationContextWithMetadata({
    trellisUrl: runtime.trellisUrl,
    sessionId: connection.loginSessionId,
    auth,
    cache: new AuthorizationContextCache(runtime.trellisUrl),
  });
  const { authenticator } = await auth.natsConnectOptions({
    sessionId: connection.loginSessionId,
    contextDigest: connection.contextDigest,
    jwt: response.routing.bootstrapJwt,
  });
  return {
    auth,
    authenticator,
    contextDigest: connection.contextDigest,
    sessionId: connection.loginSessionId,
    // The provider authorizes replies under the connection-scoped inbox prefix,
    // which is keyed by the runtime connection rather than the login session.
    inboxPrefix: `_INBOX.${connection.runtimeConnectionId}`,
  };
}
