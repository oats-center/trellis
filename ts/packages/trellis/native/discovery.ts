import { isIP } from "node:net";
import { err, ok, type Result } from "@oatscenter/result";
import { z } from "zod";
import { fromPublic } from "@nats-io/nkeys";
import type { RuntimeAuthorizationTrust } from "../auth/authority.ts";
import { base64urlDecode, base64urlEncode, sha256 } from "../auth/utils.ts";
import { codecs } from "../generated_support.ts";
import { AuthError } from "../errors/AuthError.ts";
import { boundedJson } from "../auth/oauth_core.ts";

const metadataSchema = z.object({
  issuer: z.string(),
  trellis: z.object({
    trellisInstanceId: z.string().min(1),
    audienceNatsAccount: z.string().min(1),
    natsEndpoints: z.array(z.string()).min(1),
    authorityFormat: z.literal("trellis.session-authority.v1"),
    admissionFormat: z.literal("trellis.oauth-admission.v1"),
    issuers: z
      .array(
        z.object({
          keyId: z.string().min(1),
          publicKey: z.string().min(1),
          state: z.enum(["active", "retired", "revoked"]),
        }),
      )
      .min(1),
    rotationSequence: z.string().regex(/^(0|[1-9][0-9]*)$/),
    requiredAuthorizationDetailsTypes: z
      .array(z.literal("trellis_capabilities"))
      .min(1),
  }),
});

/** Explicit selected issuer, native transport addresses, and initial trust pin. */
export type NativeOAuthServer = Readonly<{
  issuer: string;
  natsServers: readonly string[];
  trust: RuntimeAuthorizationTrust;
}>;

/** Decode a bounded OAuth response before interpreting its protocol fields. @internal */
/** Discover only the selected HTTPS origin (literal loopback HTTP is allowed), without redirects. Retained custody never repins from discovery. */
export async function discoverNativeOAuth(
  origin: string,
  options: { signal?: AbortSignal } = {},
): Promise<Result<NativeOAuthServer, AuthError>> {
  try {
    const url = new URL(origin);
    const host = url.hostname.replace(/^\[|\]$/g, "");
    const loopback =
      host === "localhost" ||
      host === "::1" ||
      (isIP(host) === 4 && host.startsWith("127."));
    if (
      url.pathname !== "/" ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      (url.protocol !== "https:" && !(url.protocol === "http:" && loopback))
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    const response = await fetch(
      `${url.origin}/.well-known/oauth-authorization-server`,
      {
        redirect: "error",
        credentials: "omit",
        signal: AbortSignal.any([
          AbortSignal.timeout(30000),
          ...(options.signal ? [options.signal] : []),
        ]),
      },
    );
    if (!response.ok) throw new AuthError({ reason: "OAuthUnavailable" });
    const metadata = metadataSchema.parse(await boundedJson(response));
    if (metadata.issuer !== url.origin)
      throw new AuthError({ reason: "InvalidIssuer" });
    codecs.ulid.decode(metadata.trellis.trellisInstanceId);
    if (
      !metadata.trellis.audienceNatsAccount.startsWith("A") ||
      fromPublic(metadata.trellis.audienceNatsAccount).getPublicKey() !==
        metadata.trellis.audienceNatsAccount
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    for (const issuer of metadata.trellis.issuers) {
      const bytes = base64urlDecode(issuer.publicKey);
      if (
        bytes.length !== 32 ||
        base64urlEncode(bytes) !== issuer.publicKey ||
        base64urlEncode(await sha256(bytes)) !== issuer.keyId
      )
        throw new AuthError({ reason: "InvalidIssuer" });
    }
    const active = metadata.trellis.issuers.filter(
      (key) => key.state === "active",
    );
    if (
      active.length !== 1 ||
      new Set(metadata.trellis.issuers.map((key) => key.keyId)).size !==
        metadata.trellis.issuers.length
    )
      throw new AuthError({ reason: "InvalidIssuer" });
    for (const server of metadata.trellis.natsEndpoints) {
      const endpoint = new URL(server);
      if (
        !["nats:", "tls:", "ws:", "wss:"].includes(endpoint.protocol) ||
        !endpoint.hostname ||
        endpoint.username ||
        endpoint.password ||
        // A WebSocket may be served at a path, such as the runtime's
        // same-origin `/nats` relay; native endpoints have none.
        (endpoint.pathname &&
          endpoint.pathname !== "/" &&
          !["ws:", "wss:"].includes(endpoint.protocol)) ||
        endpoint.search ||
        endpoint.hash
      )
        throw new AuthError({ reason: "InvalidIssuer" });
    }
    const nativeServers = metadata.trellis.natsEndpoints.filter((server) =>
      /^(nats|tls):/.test(server),
    );
    if (nativeServers.length === 0)
      throw new AuthError({ reason: "InvalidIssuer" });
    return ok({
      issuer: url.origin,
      natsServers: nativeServers,
      trust: {
        instanceId: metadata.trellis.trellisInstanceId,
        account: metadata.trellis.audienceNatsAccount,
        issuer: active[0],
        rotationSequence: metadata.trellis.rotationSequence,
      },
    });
  } catch (cause) {
    return err(
      cause instanceof AuthError
        ? cause
        : new AuthError({
            reason: options.signal?.aborted
              ? "OAuthCancelled"
              : "OAuthUnavailable",
          }),
    );
  }
}
