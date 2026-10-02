/**
 * Complete, validated broker connection inventory.
 *
 * Some transport tests must prove that one exact physical attachment
 * (`server:cid`) is gone from the broker, not merely that a digest-filtered or
 * single-reply view no longer names it. This reads the authenticated `$SYS`
 * system surface through a system-account connection: every responsive server
 * is discovered with a broadcast STATSZ, then its full authenticated CONNZ
 * list is paged, and each reply is validated against the requested server
 * before any connection is trusted.
 *
 * Absence is only ever derived from a fully validated listing. A silent,
 * denied, malformed, mismatched, truncated, or inconsistent reply — for
 * discovery or for any page — makes the inventory unavailable by throwing, so
 * it can never be mistaken for a complete inventory that happens to omit the
 * attachment under test.
 */

import type { NatsConnection } from "@nats-io/nats-core";
import { connect, credsAuthenticator } from "@nats-io/transport-node";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";

/** Marker the Trellis Auth Callout writes into the authenticated user field. */
const ATTACHMENT_MARKER_PREFIX = "trellis.auth.v1:";
const STATSZ_SUBJECT = "$SYS.REQ.SERVER.PING.STATSZ";
const CONNZ_PAGE_LIMIT = 256;
const STATSZ_WINDOW_MS = 1_000;
const SYS_REQUEST_TIMEOUT_MS = 2_000;
/** Bounded per-page CONNZ request retries before the inventory is unavailable. */
const CONNZ_REQUEST_ATTEMPTS = 3;
/** Bounded CONNZ page count, so a broker that never reaches its total stops. */
const MAX_CONNZ_PAGES = 128;

/** One broker-reported connection identity. */
export type BrokerConnection = {
  server: string;
  cid: number;
  /**
   * The broker's authenticated user field. A Trellis-admitted attachment
   * carries `trellis.auth.v1:<contextDigest>`.
   */
  authorizedUser: string;
};

/** Stable `server:cid` identity of one broker connection. */
export function brokerConnectionKey(connection: BrokerConnection): string {
  return `${connection.server}:${connection.cid}`;
}

/**
 * Additional broker identities a complete inventory must cover.
 *
 * A broadcast STATSZ is the discovery mechanism, not proof of the whole
 * cluster. A caller that already knows the exact server hosting an attachment
 * under test names it here: the server is read directly and must answer with a
 * valid, complete CONNZ listing, so the target can never disappear behind a
 * partial or dropped discovery reply.
 */
export type BrokerInventoryOptions = {
  /** Server ids that must be covered even if STATSZ does not name them. */
  requiredServerIds?: readonly string[];
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

function decodeRecord(data: Uint8Array): Record<string, unknown> | undefined {
  try {
    const parsed: unknown = JSON.parse(new TextDecoder().decode(data));
    if (
      parsed === null || typeof parsed !== "object" || Array.isArray(parsed)
    ) {
      return undefined;
    }
    return parsed as Record<string, unknown>;
  } catch {
    return undefined;
  }
}

/**
 * Discover every responsive server id through a broadcast STATSZ request.
 *
 * Discovery must be proven, not assumed: a missing, malformed, or error reply
 * fails instead of being silently omitted, and a broadcast that reaches no
 * server fails instead of returning an empty set.
 */
async function discoverServers(nc: NatsConnection): Promise<string[]> {
  const servers = new Set<string>();
  try {
    const replies = await nc.requestMany(STATSZ_SUBJECT, "", {
      strategy: "timer",
      maxWait: STATSZ_WINDOW_MS,
    });
    for await (const message of replies) {
      const reply = decodeRecord(message.data);
      if (reply === undefined) {
        throw new Error("a STATSZ reply was not a JSON object");
      }
      if (reply.error !== undefined) {
        throw new Error(
          `a STATSZ reply carried an error: ${String(reply.error)}`,
        );
      }
      const server = reply.server;
      const id = isRecord(server) ? server.id : undefined;
      if (typeof id !== "string" || id.length === 0) {
        throw new Error("a STATSZ reply carried no server identity");
      }
      servers.add(id);
    }
  } catch (cause) {
    throw new Error(
      `STATSZ server discovery failed: ${
        cause instanceof Error ? cause.message : String(cause)
      }`,
      { cause },
    );
  }
  if (servers.size === 0) {
    throw new Error("no broker answered the STATSZ server discovery request");
  }
  return [...servers];
}

/**
 * Validate one authenticated CONNZ reply page against the exact request.
 *
 * The reply must carry the real system envelope and data objects, identify the
 * requested server in both, echo the requested page cursor and limit, and
 * carry a complete, self-consistent connection array whose every record is a
 * well-formed connection. Anything else is unavailable evidence, never an
 * empty page.
 */
function validateConnzPage(
  bytes: Uint8Array,
  serverId: string,
  offset: number,
): { connections: BrokerConnection[]; total: number } {
  const reply = decodeRecord(bytes);
  if (reply === undefined) {
    throw new Error(`broker CONNZ reply for ${serverId} was not a JSON object`);
  }
  if (reply.error !== undefined) {
    throw new Error(
      `broker CONNZ reply for ${serverId} carried an error: ${
        String(reply.error)
      }`,
    );
  }
  const envelopeServer = reply.server;
  if (
    !isRecord(envelopeServer) || typeof envelopeServer.id !== "string" ||
    envelopeServer.id.length === 0
  ) {
    throw new Error(
      `broker CONNZ reply for ${serverId} carried no server envelope identity`,
    );
  }
  if (envelopeServer.id !== serverId) {
    throw new Error(
      `broker CONNZ reply envelope identified ${envelopeServer.id} but ${serverId} was requested`,
    );
  }
  const page = reply.data;
  if (!isRecord(page)) {
    throw new Error(
      `broker CONNZ reply for ${serverId} carried no data object`,
    );
  }
  if (typeof page.server_id !== "string" || page.server_id !== serverId) {
    throw new Error(
      `broker CONNZ reply data identified ${
        String(page.server_id)
      } but ${serverId} was requested`,
    );
  }
  if (!Array.isArray(page.connections)) {
    throw new Error(
      `broker CONNZ reply for ${serverId} carried no connections array`,
    );
  }
  if (!isSafeInteger(page.offset) || page.offset !== offset) {
    throw new Error(
      `broker CONNZ reply for ${serverId} was not the requested page: offset ${
        String(page.offset)
      } wanted ${offset}`,
    );
  }
  if (!isSafeInteger(page.limit) || page.limit !== CONNZ_PAGE_LIMIT) {
    throw new Error(
      `broker CONNZ reply for ${serverId} reported limit ${
        String(page.limit)
      } for a request of ${CONNZ_PAGE_LIMIT}`,
    );
  }
  const total = page.total;
  if (!isSafeInteger(total) || total < 0) {
    throw new Error(
      `broker CONNZ reply for ${serverId} carried no valid total: ${
        String(total)
      }`,
    );
  }
  if (
    !isSafeInteger(page.num_connections) ||
    page.num_connections !== page.connections.length
  ) {
    throw new Error(
      `broker CONNZ reply for ${serverId} was inconsistent: num_connections ${
        String(page.num_connections)
      } for ${page.connections.length} records`,
    );
  }
  const connections: BrokerConnection[] = [];
  for (const raw of page.connections) {
    if (!isRecord(raw)) {
      throw new Error(
        `broker CONNZ reply for ${serverId} carried a malformed connection record`,
      );
    }
    const cid = raw.cid;
    if (!isSafeInteger(cid) || cid <= 0) {
      throw new Error(
        `broker CONNZ connection record for ${serverId} carried no positive integer cid: ${
          JSON.stringify(cid)
        }`,
      );
    }
    const user = raw.authorized_user;
    // An absent authorized_user is an anonymous/control connection; a present
    // one must be a real string.
    if (user !== undefined && typeof user !== "string") {
      throw new Error(
        `broker CONNZ connection record for ${serverId} carried a non-string authorized_user`,
      );
    }
    connections.push({
      server: serverId,
      cid,
      authorizedUser: user ?? "",
    });
  }
  return { connections, total };
}

/**
 * Read and validate one authenticated CONNZ page for one server.
 *
 * Transient request failures are retried a bounded number of times. A reply
 * that arrives but fails validation is a definitive protocol failure and is
 * not retried.
 */
async function readConnzPage(
  nc: NatsConnection,
  serverId: string,
  offset: number,
): Promise<{ connections: BrokerConnection[]; total: number }> {
  const subject = `$SYS.REQ.SERVER.${serverId}.CONNZ`;
  const payload = JSON.stringify({
    auth: true,
    offset,
    limit: CONNZ_PAGE_LIMIT,
  });
  let lastError: unknown;
  for (let attempt = 1; attempt <= CONNZ_REQUEST_ATTEMPTS; attempt += 1) {
    let bytes: Uint8Array;
    try {
      bytes = (await nc.request(subject, payload, {
        timeout: SYS_REQUEST_TIMEOUT_MS,
      })).data;
    } catch (cause) {
      lastError = cause;
      continue;
    }
    return validateConnzPage(bytes, serverId, offset);
  }
  throw new Error(
    `no usable CONNZ reply for ${serverId} after ${CONNZ_REQUEST_ATTEMPTS} attempts`,
    { cause: lastError },
  );
}

/**
 * Page the complete authenticated CONNZ inventory for one server.
 *
 * The read only ends once the advertised total has been reached exactly. A
 * truncated page before the advertised total, a changed total mid-read, a
 * repeated connection id, or more records than advertised all fail: partial
 * evidence is never returned as a complete inventory.
 */
async function serverConnections(
  nc: NatsConnection,
  serverId: string,
): Promise<BrokerConnection[]> {
  const connections: BrokerConnection[] = [];
  const seenCids = new Set<number>();
  let offset = 0;
  let expectedTotal: number | undefined;
  for (let page = 0; page < MAX_CONNZ_PAGES; page += 1) {
    const parsed = await readConnzPage(nc, serverId, offset);
    if (expectedTotal === undefined) {
      expectedTotal = parsed.total;
    } else if (parsed.total !== expectedTotal) {
      throw new Error(
        `broker CONNZ inventory for ${serverId} changed size mid-read: total ${parsed.total} after ${expectedTotal}`,
      );
    }
    if (offset + parsed.connections.length > expectedTotal) {
      throw new Error(
        `broker CONNZ inventory for ${serverId} returned more connections than its advertised total ${expectedTotal}`,
      );
    }
    for (const connection of parsed.connections) {
      if (seenCids.has(connection.cid)) {
        throw new Error(
          `broker CONNZ inventory for ${serverId} repeated cid ${connection.cid}`,
        );
      }
      seenCids.add(connection.cid);
      connections.push(connection);
    }
    offset += parsed.connections.length;
    if (offset === expectedTotal) return connections;
    if (parsed.connections.length === 0) {
      throw new Error(
        `broker CONNZ inventory for ${serverId} ended at ${offset} of ${expectedTotal} connections`,
      );
    }
  }
  throw new Error(
    `broker CONNZ inventory for ${serverId} exceeded ${MAX_CONNZ_PAGES} pages`,
  );
}

/**
 * Read the complete authenticated connection inventory of every responsive
 * server, plus any {@link BrokerInventoryOptions.requiredServerIds}.
 *
 * The system account is required. A caller without system permissions, a
 * silent broker, or any malformed or incomplete reply makes the whole
 * inventory unavailable by throwing, so an absent attachment can only be
 * concluded from a fully validated listing.
 */
export async function completeBrokerInventory(
  nc: NatsConnection,
  options: BrokerInventoryOptions = {},
): Promise<BrokerConnection[]> {
  const required = options.requiredServerIds ?? [];
  for (const serverId of required) {
    if (typeof serverId !== "string" || serverId.length === 0) {
      throw new Error("required broker server ids must be non-empty strings");
    }
  }
  const requested: string[] = [];
  const seen = new Set<string>();
  for (const serverId of [...(await discoverServers(nc)), ...required]) {
    if (seen.has(serverId)) continue;
    seen.add(serverId);
    requested.push(serverId);
  }
  const inventory: BrokerConnection[] = [];
  for (const serverId of requested) {
    inventory.push(...(await serverConnections(nc, serverId)));
  }
  return inventory;
}

/**
 * Read the complete validated broker inventory using the runtime's own system
 * credentials.
 *
 * This is the ordinary entry point for a test that already owns a runtime: it
 * opens an ordinary node NATS connection to the runtime's broker, pins that
 * connection's own advertised server as required coverage, reads the canonical
 * {@link completeBrokerInventory}, and closes the connection. A missing server
 * identity or any unusable reply throws rather than returning an empty
 * inventory.
 */
export async function readRuntimeBrokerInventory(
  runtime: Pick<TrellisTestRuntime, "workdir" | "natsUrl">,
  options: BrokerInventoryOptions = {},
): Promise<BrokerConnection[]> {
  const creds = await Deno.readTextFile(
    `${runtime.workdir}/nats/creds/system.creds`,
  );
  const connection = await connect({
    servers: runtime.natsUrl,
    authenticator: credsAuthenticator(new TextEncoder().encode(creds)),
  });
  try {
    const serverId = connection.info?.server_id;
    if (typeof serverId !== "string" || serverId.length === 0) {
      throw new Error(
        "the system connection reported no broker server identity",
      );
    }
    const required = new Set<string>([
      serverId,
      ...(options.requiredServerIds ?? []),
    ]);
    return await completeBrokerInventory(connection, {
      requiredServerIds: [...required],
    });
  } finally {
    await connection.close();
  }
}

/** The admitted sockets whose authorization-context digest is in `digests`. */
export function admittedConnections(
  inventory: readonly BrokerConnection[],
  digests: ReadonlySet<string>,
): BrokerConnection[] {
  return inventory.filter((connection) => {
    if (!connection.authorizedUser.startsWith(ATTACHMENT_MARKER_PREFIX)) {
      return false;
    }
    // The authenticated user is
    // `trellis.auth.v1:<contextDigest>:<serverId>:<cid>`: match the digest
    // field, not a prefix of the whole marker.
    const rest = connection.authorizedUser.slice(
      ATTACHMENT_MARKER_PREFIX.length,
    );
    const digest = rest.split(":", 1)[0];
    return digests.has(digest);
  });
}
