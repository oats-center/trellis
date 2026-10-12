import {
  BrowserOAuthClient,
  type BrowserOAuthAdmission,
} from "../packages/trellis/auth/browser.ts";
import { apps } from "../packages/trellis-testkit/trellis/index.js";

let client: BrowserOAuthClient | undefined;

/** Open the public browser client against the real selected OAuth origin. */
export async function open(issuer: string, handle: string) {
  client?.store.close();
  client = (
    await BrowserOAuthClient.open({
      issuer,
      clientId: "issuer-rotation-browser",
      credentialHandle: handle,
    })
  ).orThrow();
}

/** Start ordinary PKCE using selections from the generated CLI contract. */
export async function authorize() {
  if (!client) throw new Error("open browser custody first");
  return (
    await client.authorize(`${location.origin}/callback`, {
      required: apps.cli.request.requiredCapabilities,
      optional: [],
    })
  ).orThrow();
}

function installed(admission: BrowserOAuthAdmission) {
  const credential = admission.verifiedAdmission.credential;
  return {
    principalId: credential.principalId,
    grantId: credential.oauthGrantId,
    loginId: credential.loginSessionId,
    dpop: credential.persistentDpopJkt,
    runtime: admission.runtimeId,
    nkey: admission.auth.sessionNkey,
    keyId: admission.trust.issuer.keyId,
    rotationSequence: admission.trust.rotationSequence,
  };
}

/** Complete and persist only the real issuer-verified authorization-code grant. */
export async function complete() {
  if (!client) throw new Error("open browser custody first");
  const result = (await client.complete()).orThrow();
  const credential = result.value.verifiedAdmission.credential;
  (
    await client.store.rememberGrant({
      principalId: credential.principalId,
      oauthGrantId: credential.oauthGrantId,
      loginSessionId: credential.loginSessionId,
      refreshToken: result.refreshToken,
    })
  ).orThrow();
  return installed(result.value);
}

/** Exercise the supported install callback after signed admission verification. */
export async function resume() {
  if (!client) throw new Error("open browser custody first");
  let admission: BrowserOAuthAdmission | undefined;
  const result = await client.resume(async (value) => {
    admission = value;
  });
  if (result.isErr()) return { error: result.error.reason };
  if (!admission) throw new Error("verified admission was not installed");
  return installed(admission);
}

/** Compare custody outcomes without returning credentials or private key material. */
export async function tokenDigest() {
  if (!client) throw new Error("open browser custody first");
  const token = (await client.store.refreshToken()).orThrow();
  if (!token) throw new Error("missing browser refresh credential");
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(token),
  );
  return [...new Uint8Array(digest)]
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}
