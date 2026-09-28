import type { Authenticator } from "@nats-io/nats-core";
import {
  AuthorizationContextCache,
  createAuth,
  refreshAuthorizationContextWithMetadata,
} from "@oatscenter/trellis/auth";
import type {
  TrellisTestClientKey,
  TrellisTestRuntime,
} from "@oatscenter/trellis-testkit";

/**
 * Metadata of a registered client's existing admitted runtime connection.
 *
 * Reading this needs no private key: it is the connection the client already
 * holds, discovered from public admin state.
 */
export type ObservedClientConnection = {
  /** Durable login session the connection was admitted under. */
  loginSessionId: string;
  /** Server-assigned runtime connection identity. */
  runtimeConnectionId: string;
  /** Currently admitted authorization-context digest. */
  contextDigest: string;
  /** Exact authenticated inbox prefix accepted for replies. */
  inboxPrefix: string;
};

/** A complete, independent raw user runtime connection. */
export type RawClientConnection = {
  /** Runtime credential whose key is bound into this connection's context. */
  auth: Awaited<ReturnType<typeof createAuth>>;
  /** Issued NATS authenticator for this connection. */
  authenticator: Authenticator | Authenticator[];
  /** Authorization-context digest issued for this connection. */
  contextDigest: string;
  /** Server-assigned runtime connection identity. */
  connectionId: string;
  /** Exact authenticated inbox prefix accepted for replies. */
  inboxPrefix: string;
  /** Durable login session this connection was issued under. */
  loginSessionId: string;
};

/**
 * Observes an existing admitted runtime connection from public admin state.
 *
 * A runtime connection's ephemeral key is not recoverable, so an existing
 * connection is observed through its metadata rather than reconstructed.
 */
export async function observeClientConnection(
  runtime: TrellisTestRuntime,
  key: TrellisTestClientKey,
): Promise<ObservedClientConnection> {
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
  return {
    loginSessionId: connection.loginSessionId,
    runtimeConnectionId: connection.runtimeConnectionId,
    contextDigest: connection.contextDigest,
    inboxPrefix: `_INBOX.${connection.runtimeConnectionId}`,
  };
}

/**
 * Issues a complete raw user runtime connection for a registered client's
 * durable login, through the production context-refresh endpoint.
 *
 * The connection is its own logical runtime connection: it carries the freshly
 * issued context digest and inbox prefix, and never mixes metadata from the
 * client's separate facade connection.
 */
export async function issueRawClientConnection(
  runtime: TrellisTestRuntime,
  key: TrellisTestClientKey,
  loginSessionId: string,
): Promise<RawClientConnection> {
  let contextDigest = "";
  const auth = await createAuth({
    sessionKeySeed: key.seed,
    contextDigest: () => contextDigest,
  });
  const { response, context } = await refreshAuthorizationContextWithMetadata({
    trellisUrl: runtime.trellisUrl,
    credential: { loginSessionId, proofAuth: auth },
    runtime: { auth },
    cache: new AuthorizationContextCache(runtime.trellisUrl),
  });
  contextDigest = context.contextDigest;
  const { authenticator } = await auth.natsConnectOptions({
    inboxPrefix: response.runtime.inboxPrefix,
    contextDigest: context.contextDigest,
    jwt: response.routing.bootstrapJwt,
  });
  return {
    auth,
    authenticator,
    contextDigest: context.contextDigest,
    connectionId: response.runtime.connectionId,
    inboxPrefix: response.runtime.inboxPrefix,
    loginSessionId,
  };
}
