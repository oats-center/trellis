import { isErr } from "@oatscenter/result";
import type { AppRequestDescriptor } from "@oatscenter/trellis/generated";
import {
  NativeOAuthClient,
  discoverNativeOAuth,
  type NativeOAuthCustody,
} from "@oatscenter/trellis/native";
import type {
  NativeOAuthSession,
  PlatformPrivilege,
} from "@oatscenter/trellis/native";
import { ulid } from "ulid";
import { z } from "zod";

const presentationSchema = z.object({
  clientId: z.string(),
  deviceUserCode: z.string(),
  presentationId: z.string(),
  capabilities: z.array(
    z.object({
      capabilityId: z.string(),
      available: z.boolean(),
      required: z.boolean(),
    }),
  ),
  platformPrivileges: z.array(
    z.object({
      privilege: z.string(),
      available: z.boolean(),
      required: z.boolean(),
    }),
  ),
});

/** Complete public native Device Code, real Portal local login, and explicit consent; no browser realm or injected authority. Node-only. */
export async function loginNativeTestClient(options: {
  issuer: string;
  clientId: string;
  /** Reauthorize an existing custody handle instead of opening a fresh client. */
  client?: NativeOAuthClient;
  /** Explicit process-local or application-owned custody for reconnecting this exact family. */
  custody?: NativeOAuthCustody;
  request: AppRequestDescriptor;
  username: string;
  password: string;
  /** Exact real consent selections; omitted approves all available capabilities. Required capabilities cannot be declined. */
  approvedCapabilities?: readonly string[];
  platformPrivileges?: {
    required: readonly PlatformPrivilege[];
    optional?: readonly PlatformPrivilege[];
  };
  signal?: AbortSignal;
}): Promise<{ session: NativeOAuthSession; servers: readonly string[] }> {
  const server = (
    await discoverNativeOAuth(options.issuer, { signal: options.signal })
  ).take();
  if (isErr(server)) throw server.error;
  const client =
    options.client ??
    (
      await NativeOAuthClient.open({
        custody: options.custody,
        scope: {
          issuer: server.issuer,
          instanceId: server.trust.instanceId,
          account: server.trust.account,
          clientId: options.clientId,
          credentialHandle: ulid(),
        },
        trust: server.trust,
        signal: options.signal,
      })
    ).take();
  if (isErr(client)) throw client.error;
  try {
    const device = (
      await client.startDevice(options.request, {
        platformPrivileges: options.platformPrivileges,
        signal: options.signal,
      })
    ).take();
    if (isErr(device)) throw device.error;
    await approveNativeDeviceCode({
      ...options,
      issuer: server.issuer,
      userCode: device.userCode,
    });
    const session = (await device.complete({ signal: options.signal })).take();
    if (isErr(session)) throw session.error;
    return { session, servers: server.natsServers };
  } catch (error) {
    await client.close();
    throw error;
  }
}

/** Approve an independently owned native Device Code through real Portal login and explicit consent. */
export async function approveNativeDeviceCode(options: {
  issuer: string;
  clientId: string;
  userCode: string;
  username: string;
  password: string;
  approvedCapabilities?: readonly string[];
  signal?: AbortSignal;
}): Promise<void> {
  const signal = AbortSignal.any([
    AbortSignal.timeout(30000),
    ...(options.signal ? [options.signal] : []),
  ]);
  const cookies = new Map<string, string>();
  const request = async (
    path: string,
    form?: URLSearchParams,
  ): Promise<Response> => {
    const response = await fetch(new URL(path, options.issuer), {
      method: form ? "POST" : "GET",
      body: form,
      headers: {
        Origin: options.issuer,
        Cookie: [...cookies]
          .map(([key, value]) => `${key}=${value}`)
          .join("; "),
      },
      redirect: "manual",
      signal,
    });
    for (const cookie of response.headers.getSetCookie()) {
      const pair = cookie.split(";", 1)[0];
      const index = pair.indexOf("=");
      if (index > 0) cookies.set(pair.slice(0, index), pair.slice(index + 1));
    }
    return response;
  };
  const portal = await request("/portal/session", new URLSearchParams());
  if (!portal.ok) throw new Error("Portal session denied");
  await portal.body?.cancel();
  const csrf = cookies.get("__Host-trellis-portal-csrf");
  if (!csrf) throw new Error("Portal session omitted CSRF cookie");
  const login = await request(
    "/portal/login",
    new URLSearchParams({
      csrf,
      username: options.username,
      password: options.password,
      request_id: ulid(),
    }),
  );
  if (!login.ok) throw new Error("Portal local login denied");
  await login.body?.cancel();
  const confirmation = await request(
    "/portal/device",
    new URLSearchParams({ csrf, user_code: options.userCode }),
  );
  const location = confirmation.headers.get("Location");
  if (confirmation.status < 300 || confirmation.status >= 400 || !location)
    throw new Error("Device confirmation denied");
  await confirmation.body?.cancel();
  const consent = new URL(location, options.issuer);
  const transaction = consent.searchParams.get("transaction");
  if (consent.origin !== options.issuer || !transaction)
    throw new Error("Invalid consent location");
  const response = await request(
    `/portal/authorization/${encodeURIComponent(transaction)}`,
  );
  if (!response.ok) throw new Error("Consent presentation denied");
  const view = presentationSchema.parse(await response.json());
  if (
    view.clientId !== options.clientId ||
    view.deviceUserCode !== options.userCode
  )
    throw new Error("Consent identity mismatch");
  const form = new URLSearchParams({
    csrf,
    transaction,
    presentation: view.presentationId,
    decision: "approve",
    confirm_device: options.userCode,
  });
  for (const item of view.capabilities) {
    if (item.required && !item.available)
      throw new Error("Required capability unavailable");
    const approved =
      !options.approvedCapabilities ||
      options.approvedCapabilities.includes(item.capabilityId);
    if (item.required && !approved)
      throw new Error("Required capability cannot be declined");
    if (item.available && approved) form.append("approved", item.capabilityId);
  }
  for (const item of view.platformPrivileges) {
    if (item.required && !item.available)
      throw new Error("Required privilege unavailable");
    if (item.available) form.append("privilege", item.privilege);
  }
  const approved = await request("/portal/approve", form);
  if (
    approved.status < 300 ||
    approved.status >= 400 ||
    approved.headers.get("Location") !== "/device/complete?decision=approved"
  )
    throw new Error("Device consent did not complete");
  await approved.body?.cancel();
}
