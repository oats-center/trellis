import { AuthError } from "../errors/AuthError.ts";
import { issuerChainSchema, signedBytes } from "./protocol_wasm.ts";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "./authority.ts";

/** Authenticate a discovery hint from the credential's existing pin, without consuming OAuth credentials. */
export async function authenticateIssuerChain(
  origin: string,
  anchor: RuntimeAuthorizationTrust,
  target: RuntimeAuthorizationTrust,
  signal?: AbortSignal,
  existingOwner?: SessionAuthorityOwner,
): Promise<SessionAuthorityOwner> {
  const owner =
    existingOwner ??
    new SessionAuthorityOwner({
      instanceId: anchor.instanceId,
      account: anchor.account,
      issuers: [anchor.issuer],
      rotationSequence: anchor.rotationSequence,
    });
  try {
    if (
      anchor.instanceId !== target.instanceId ||
      anchor.account !== target.account ||
      BigInt(target.rotationSequence) < BigInt(anchor.rotationSequence)
    )
      throw new Error("issuer scope or sequence mismatch");
    if (anchor.issuer.keyId !== target.issuer.keyId) {
      const url = new URL("/.well-known/trellis/issuer-chain", origin);
      url.search = new URLSearchParams({
        pinnedKeyId: anchor.issuer.keyId,
        targetKeyId: target.issuer.keyId,
      }).toString();
      const response = await fetch(url, {
        credentials: "omit",
        redirect: "error",
        signal: AbortSignal.any([
          AbortSignal.timeout(30000),
          ...(signal ? [signal] : []),
        ]),
      });
      if (!response.ok || !response.body)
        throw new Error("issuer chain unavailable");
      const reader = response.body.getReader();
      const chunks: Uint8Array[] = [];
      let length = 0;
      try {
        while (true) {
          const next = await reader.read();
          if (next.done) break;
          length += next.value.length;
          if (length > 1024 * 1024)
            throw new Error("issuer chain budget exceeded");
          chunks.push(next.value);
        }
      } finally {
        await reader.cancel();
      }
      const bytes = new Uint8Array(length);
      let offset = 0;
      for (const chunk of chunks) {
        bytes.set(chunk, offset);
        offset += chunk.length;
      }
      const chain = issuerChainSchema.parse(
        JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)),
      );
      for (const rotation of chain.rotations)
        await owner.acceptIssuerRotation(signedBytes(rotation));
    }
    const verified = owner.trustForCredential(anchor);
    if (
      verified.issuer.keyId !== target.issuer.keyId ||
      verified.issuer.publicKey !== target.issuer.publicKey ||
      verified.rotationSequence !== target.rotationSequence
    )
      throw new Error("issuer chain does not reach discovery target");
    return owner;
  } catch {
    if (!existingOwner) owner.stop();
    throw new AuthError({ reason: "OAuthTrustUnavailable" });
  }
}
