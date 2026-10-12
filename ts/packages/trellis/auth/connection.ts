import type { NatsConnection } from "@nats-io/nats-core";
import { z } from "zod";
import { SessionAuthorityOwner } from "./authority.ts";
import { requestOwnAdmission } from "./admission.ts";
import { AuthError } from "../errors/AuthError.ts";
import { TransportError } from "../errors/TransportError.ts";
import { createAuth, type TrellisAuth } from "./session_auth.ts";
import { base64urlDecode, base64urlEncode, sha256, utf8 } from "./utils.ts";
import {
  signProvisionedAdmissionWasm,
  verifyOAuthAdmissionWasm,
  createSessionAuthorityHandleWasm,
  assertSessionAuthorityHandleCurrentWasm,
  transportAuthorizationDigestWasm,
  classifyTransportAuthorizationWasm,
  type AuthorityIssuerKey,
  type AdmissionContext,
  type TransportAuthorizationV1,
  type OAuthAdmission,
} from "./protocol_wasm.ts";
import {
  loadDefaultRuntimeTransport,
  type RuntimeTransport,
} from "../runtime_transport.ts";
import {
  TransportGenerationManager,
  type TransportGeneration,
  type TransportGenerationPrepared,
  type TransportGenerationConnect,
  type OwnAdmission,
} from "../transport/generations.ts";

/** Provisioned connectivity and trust; never principal, deployment, participant or implementation assertions. */
export type ProvisionedConnectionOptions = {
  servers: string[];
  trellisInstanceId: string;
  audienceNatsAccount: string;
  issuers: readonly AuthorityIssuerKey[];
  seed: string;
  timeout?: number;
  signal?: AbortSignal;
};

const policySchema = z.object({
  format: z.literal("trellis.transport-authorization.v1"),
  account: z.string(),
  publishAllow: z.array(z.string()),
  subscribeAllow: z.array(z.string()),
  response: z
    .object({ maxMessages: z.number().int(), ttlMs: z.number().int() })
    .nullable(),
  hardExpiresAt: z.number().int().nullable(),
});
const registrySchema = z.object({
  materialBucket: z.string(),
  revocationBucket: z.string(),
  issuer: z.object({
    keyId: z.string(),
    publicKey: z.string(),
    state: z.enum(["active", "retired", "revoked"]),
  }),
  rotationSequence: z.string(),
  transportAuthorization: policySchema,
  admittedTransportAuthorization: policySchema,
  attachmentId: z.string(),
});

/** Direct service/device CONNECT with an identity-signed credential and a fresh runtime key. */
export async function connectProvisioned(
  options: ProvisionedConnectionOptions,
  kind: "service" | "device",
  hooks: {
    onPreActivate?(generation: TransportGeneration): Promise<void>;
    onDrain?(generation: TransportGeneration): void;
  } = {},
  transport?: RuntimeTransport,
) {
  const runtimeId = base64urlEncode(crypto.getRandomValues(new Uint8Array(16)));
  let owner: SessionAuthorityOwner | undefined;
  const auth = await createAuth({
    sessionKeySeed: base64urlEncode(crypto.getRandomValues(new Uint8Array(32))),
    contextDigest: () => owner?.ownDigest ?? "",
  });
  const identity = await createAuth({ sessionKeySeed: options.seed });
  const credentialId = base64urlEncode(
    await sha256(base64urlDecode(identity.sessionKey)),
  );
  const connected = await connectAdmitted({
    ...options,
    auth,
    runtimeId,
    kind,
    hooks,
    transport,
    admission: async () => {
      const issuedAt = auth.currentIat();
      return await signProvisionedAdmissionWasm(
        {
          format: "trellis.provisioned-admission.v1",
          trellisInstanceId: options.trellisInstanceId,
          audienceNatsAccount: options.audienceNatsAccount,
          credentialId,
          runtimeId,
          ephemeralNkey: auth.sessionNkey,
          issuedAt,
          expiresAt: issuedAt + 60,
        },
        base64urlDecode(options.seed),
      );
    },
  });
  owner = connected.owner;
  return connected;
}

/** One signed-admission connection path for OAuth and provisioned runtimes. */
export async function connectAdmitted(options: {
  servers: string[];
  trellisInstanceId: string;
  audienceNatsAccount: string;
  issuers: readonly AuthorityIssuerKey[];
  rotationSequence?: string;
  auth: TrellisAuth;
  runtimeId: string;
  kind: "user" | "service" | "device";
  timeout?: number;
  signal?: AbortSignal;
  admission(): Promise<string>;
  transport?: RuntimeTransport;
  hooks?: {
    onPreActivate?(generation: TransportGeneration): Promise<void>;
    onDrain?(generation: TransportGeneration): void;
  };
}) {
  const owner = new SessionAuthorityOwner({
    instanceId: options.trellisInstanceId,
    account: options.audienceNatsAccount,
    issuers: options.issuers,
    rotationSequence: options.rotationSequence,
  });
  options.auth.authorityOwner = owner;
  const transport = options.transport ?? (await loadDefaultRuntimeTransport());
  const inboxPrefix = `_INBOX.trellis.${base64urlEncode(await sha256(base64urlDecode(options.auth.sessionKey)))}`;
  let desired: TransportAuthorizationV1 | undefined;
  let installed: AdmissionContext | undefined;
  let preparationEpoch = 0;
  let renewalTimer: ReturnType<typeof setTimeout> | undefined;
  let renewalAbort: AbortController | undefined;
  let renewalTask: Promise<void> | undefined;
  const credentials = new WeakMap<NatsConnection, string>();
  const preparedCredentials = new WeakMap<TransportGenerationConnect, string>();
  const pendingAdmissions = new WeakMap<
    NatsConnection,
    Promise<OwnAdmission>
  >();
  const candidates = new WeakMap<
    NatsConnection,
    {
      context: AdmissionContext;
      policy: TransportAuthorizationV1;
      admission: OwnAdmission;
      candidate?: Awaited<ReturnType<SessionAuthorityOwner["prepareOwn"]>>;
      committed: boolean;
      clientId: number | undefined;
      serverId: string | undefined;
      oauth?: OAuthAdmission;
      dispose(): void;
    }
  >();
  const readAdmission = async (
    nc: NatsConnection,
    fresh = false,
    signal = options.signal,
  ): Promise<OwnAdmission> => {
    const existing = candidates.get(nc);
    if (existing && !fresh) {
      if (
        existing.clientId !== nc.info?.client_id ||
        existing.serverId !== nc.info?.server_id
      )
        throw new Error("AdmissionAttachmentChanged");
      return existing.admission;
    }
    const pending = pendingAdmissions.get(nc);
    if (pending) return pending;
    const lookup = (async () => {
      const epoch = ++preparationEpoch;
      const originalDigest = owner.ownDigest;
      const credential = credentials.get(nc);
      if (!credential) throw new Error("AdmissionUnavailable");
      let oauth: OAuthAdmission | undefined = existing?.oauth;
      if (options.kind === "user" && !oauth) {
        const verified = await verifyOAuthAdmissionWasm(utf8(credential), {
          issuer: owner.trustSnapshot.issuer,
          trellisInstanceId: options.trellisInstanceId,
          audienceNatsAccount: options.audienceNatsAccount,
          ephemeralNkey: options.auth.sessionNkey,
          nowUnixSeconds: options.auth.currentIat(),
          maximumLifetimeSeconds: 300,
        });
        if (
          !verified.ok ||
          verified.admission.credential.runtimeId !== options.runtimeId
        )
          throw new Error("AdmissionBindingMismatch");
        oauth = verified.admission.credential;
      }
      // The broker marker selects a socket, not authority: Auth must bind it to
      // the proved runtime/key and certify the same attachment in its reply.
      const attachmentInfo = await nc.request("$SYS.REQ.USER.INFO", undefined, {
        timeout: options.timeout ?? 30_000,
      });
      const attachmentMarker = attachmentInfo.json<{
        data?: { user?: unknown };
      }>().data?.user;
      if (
        typeof attachmentMarker !== "string" ||
        !/^trellis\.auth\.v1:[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(
          attachmentMarker,
        )
      )
        throw new Error("AdmissionAttachmentMismatch");
      const attachmentId = attachmentMarker.slice("trellis.auth.v1:".length);
      const context = await requestOwnAdmission({
        nc,
        owner,
        auth: options.auth,
        runtimeId: options.runtimeId,
        attachmentId,
        timeoutMs: options.timeout ?? 30_000,
        signal,
      });
      const registry = registrySchema.parse(context.registry);
      // Broker diagnostics only correlate the physical attachment. Authority and
      // policy still come exclusively from the issuer-signed pending reply.
      const info = await nc.request("$SYS.REQ.USER.INFO", undefined, {
        timeout: options.timeout ?? 30_000,
      });
      const broker = info.json<{ data?: { user?: unknown } }>();
      if (
        registry.attachmentId !== attachmentId ||
        broker.data?.user !== `trellis.auth.v1:${registry.attachmentId}` ||
        registry.transportAuthorization.account !==
          options.audienceNatsAccount ||
        registry.admittedTransportAuthorization.account !==
          options.audienceNatsAccount
      )
        throw new Error("AdmissionAttachmentMismatch");
      const verified = await createSessionAuthorityHandleWasm({
        authorityBytes: utf8(context.sessionAuthority),
        verification: {
          issuer: owner.trustSnapshot.issuer,
          trellisInstanceId: options.trellisInstanceId,
          audienceNatsAccount: options.audienceNatsAccount,
          policy: owner.policy(),
          purpose: "live",
          revocationCutoff: null,
        },
      });
      const authority = verified.verified.authority;
      try {
        if (
          authority.runtimeId !== options.runtimeId ||
          authority.sessionPublicKey !== options.auth.sessionKey ||
          authority.inboxPrefix !== inboxPrefix ||
          (oauth
            ? authority.principalId !== oauth.principalId ||
              authority.oauthGrantId !== oauth.oauthGrantId ||
              authority.loginSessionId !== oauth.loginSessionId ||
              authority.binding.kind !== oauth.binding.bindingKind ||
              ((authority.binding.kind === "browser" ||
                authority.binding.kind === "native") &&
                authority.binding.clientId !== oauth.clientId) ||
              (authority.binding.kind === "browser" &&
                oauth.binding.bindingKind === "browser" &&
                authority.binding.origin !== oauth.binding.browserOrigin) ||
              (authority.binding.kind === "native" &&
                oauth.binding.bindingKind === "native" &&
                authority.binding.durableDpopJkt !==
                  oauth.binding.nativeDpopJkt)
            : authority.binding.kind !== options.kind)
        )
          throw new Error("AdmissionBindingMismatch");
        const policyChanged =
          !desired ||
          (await transportAuthorizationDigestWasm(desired)) !==
            (await transportAuthorizationDigestWasm(
              registry.transportAuthorization,
            ));
        const assertCurrent = () => {
          if (
            epoch !== preparationEpoch ||
            owner.ownDigest !== originalDigest ||
            signal?.aborted ||
            nc.isClosed() ||
            owner.trustSnapshot.issuer.keyId !== authority.issuerKeyId
          )
            throw new Error("AdmissionSuperseded");
          assertSessionAuthorityHandleCurrentWasm(
            verified.handle,
            owner.policy(),
            owner.isRevoked(authority.authorizationSessionId),
          );
          const value = candidates.get(nc);
          if (value && !value.committed)
            value.candidate?.assertPreparedCurrent();
        };
        const admission = {
          attachmentId: registry.attachmentId,
          contextDigest: verified.verified.authorityDigest,
          policy: registry.admittedTransportAuthorization,
          desiredPolicy: registry.transportAuthorization,
          assertCurrent,
        };
        assertCurrent();
        let disposed = false;
        const value = {
          context,
          policy:
            !policyChanged && desired
              ? desired
              : registry.transportAuthorization,
          admission,
          committed: false,
          clientId: nc.info?.client_id,
          serverId: nc.info?.server_id,
          oauth,
          candidate: undefined as
            | Awaited<ReturnType<SessionAuthorityOwner["prepareOwn"]>>
            | undefined,
          dispose: () => {
            if (disposed) return;
            disposed = true;
            value.candidate?.dispose();
            verified.handle.free();
          },
        };
        candidates.set(nc, value);
        existing?.dispose();
        if (!installed && policyChanged)
          desired = registry.transportAuthorization;
        void nc.closed().then(value.dispose);
        return admission;
      } catch (error) {
        verified.handle.free();
        throw error;
      }
    })();
    pendingAdmissions.set(nc, lookup);
    try {
      return await lookup;
    } finally {
      pendingAdmissions.delete(nc);
    }
  };
  const prepare = async (): Promise<
    TransportGenerationPrepared | undefined
  > => {
    if (!desired) return undefined;
    const policy = desired;
    const admission = await options.admission();
    const connect = await options.auth.natsConnectOptions({
      inboxPrefix,
      admission,
    });
    const prepared: TransportGenerationPrepared = {
      policy,
      policyDigest: await transportAuthorizationDigestWasm(policy),
      connect: {
        servers: options.servers,
        authenticators: Array.isArray(connect.authenticator)
          ? connect.authenticator
          : [connect.authenticator],
        inboxPrefix,
        maxReconnectAttempts: 0,
        timeoutMs: options.timeout ?? 30_000,
      },
    };
    preparedCredentials.set(prepared.connect, admission);
    return prepared;
  };
  const manager = new TransportGenerationManager({
    kind: options.kind,
    nowSeconds: options.auth.currentIat,
    desiredPolicy: () => desired,
    prepare,
    readAdmission,
    open: async (connect) => {
      const nc = await transport.connect({
        servers: connect.servers,
        authenticator: connect.authenticators,
        inboxPrefix,
        maxReconnectAttempts: 0,
        reconnect: false,
        timeout: connect.timeoutMs,
      });
      const credential = preparedCredentials.get(connect);
      if (!credential) {
        await nc.close();
        throw new Error("AdmissionUnavailable");
      }
      credentials.set(nc, credential);
      return nc;
    },
    onPreActivate: async (generation) => {
      const value = candidates.get(generation.nc);
      if (!value) throw new Error("AdmissionUnavailable");
      await owner.attach(
        generation.nc,
        registrySchema.parse(value.context.registry),
        { signal: options.signal, timeoutMs: options.timeout, inboxPrefix },
      );
      if (!value.committed) {
        value.candidate?.dispose();
        value.candidate = await owner.prepareOwn(
          utf8(value.context.sessionAuthority),
          {
            runtimeId: options.runtimeId,
            sessionPublicKey: options.auth.sessionKey,
          },
        );
      }
      await options.hooks?.onPreActivate?.(generation);
    },
    onDrain: options.hooks?.onDrain,
    onActivate: (generation) => {
      const value = candidates.get(generation.nc);
      if (!value) throw new Error("AdmissionUnavailable");
      if (value.committed) return;
      if (!value.candidate) throw new Error("AdmissionUnavailable");
      value.candidate.commit();
      value.committed = true;
      const policyChanged = desired !== value.policy;
      desired = value.policy;
      installed = value.context;
      if (policyChanged) manager.authorizationPromoted();
      scheduleRenewal();
    },
  });
  const scheduleRenewal = () => {
    if (renewalTimer !== undefined) clearTimeout(renewalTimer);
    renewalTimer = undefined;
    if (manager.isClosed() || options.signal?.aborted) return;
    const own = owner.own();
    let delay: number;
    try {
      delay =
        (own.authority.expiresAt -
          options.auth.currentIat() -
          60 -
          (crypto.getRandomValues(new Uint32Array(1))[0] % 16)) *
        1000;
    } finally {
      own.release();
    }
    if (delay <= 0) {
      const error = new AuthError({
        reason: "OAuthRecoveryRequired",
        message:
          "The root authority deadline leaves no bounded renewal window.",
      });
      void manager.terminate(error);
      throw error;
    }
    renewalTimer = setTimeout(() => {
      renewalTimer = undefined;
      void refreshAuthority().catch(() => {
        /* typed failure is retained by closed() */
      });
    }, delay);
  };
  /** Revalidate current authority through the admitted runtime key without rotating OAuth custody or replacing an equal-policy attachment. */
  const refreshAuthority = (): Promise<void> => {
    if (renewalTask) return renewalTask;
    renewalAbort = new AbortController();
    const signal = options.signal
      ? AbortSignal.any([options.signal, renewalAbort.signal])
      : renewalAbort.signal;
    const task = (async () => {
      const lease = await manager.acquirePublishedAttachment();
      if (!lease)
        throw new TransportError({
          code: "trellis.transport.unavailable",
          message: "No covered attachment is available for authority renewal.",
          hint: "Reconnect when the runtime is available.",
        });
      try {
        const previous = owner.own();
        const logicalSession = previous.authority.authorizationSessionId;
        previous.release();
        await readAdmission(lease.nc, true, signal);
        const value = candidates.get(lease.nc);
        if (!value) throw new Error("AdmissionUnavailable");
        value.candidate = await owner.prepareOwn(
          utf8(value.context.sessionAuthority),
          {
            runtimeId: options.runtimeId,
            sessionPublicKey: options.auth.sessionKey,
          },
        );
        const candidate = value.candidate;
        if (candidate.authority.authorizationSessionId !== logicalSession)
          throw new AuthError({
            reason: "OAuthRecoveryRequired",
            message: "Authority renewal retired the previous logical session.",
          });
        const relation = await classifyTransportAuthorizationWasm(
          value.admission.policy,
          value.policy,
          options.auth.currentIat(),
        );
        if (relation === "reduction_required")
          throw new AuthError({
            reason: "OAuthRecoveryRequired",
            message: "Authority reduction requires a new admitted attachment.",
          });
        const committed = await manager.commitOnPublished(
          lease.generationId,
          () => {
            if (signal.aborted) return false;
            value.admission.assertCurrent?.();
            candidate.assertPreparedCurrent();
            candidate.commit();
            value.committed = true;
            desired = value.policy;
            installed = value.context;
            return true;
          },
        );
        if (!committed) throw new Error("AdmissionSuperseded");
        manager.authorizationPromoted();
        scheduleRenewal();
      } finally {
        lease.release();
      }
    })();
    renewalTask = task
      .catch(async (error) => {
        if (!signal.aborted && !manager.isClosed()) {
          await manager.terminate(
            error instanceof AuthError || error instanceof TransportError
              ? error
              : new TransportError({
                  code: "trellis.transport.unavailable",
                  message: "Authority renewal is unavailable.",
                  hint: "Reconnect when the runtime is available.",
                  cause: error,
                }),
          );
        }
        throw error;
      })
      .finally(() => {
        renewalTask = undefined;
        renewalAbort = undefined;
      });
    return renewalTask;
  };
  let nc: NatsConnection | undefined;
  try {
    if (options.signal?.aborted) throw options.signal.reason;
    const credential = await options.admission();
    const authOptions = await options.auth.natsConnectOptions({
      inboxPrefix,
      admission: credential,
    });
    nc = await transport.connect({
      servers: options.servers,
      ...authOptions,
      maxReconnectAttempts: 0,
      timeout: options.timeout ?? 30_000,
    });
    credentials.set(nc, credential);
    const admission = await readAdmission(nc);
    const prepared: TransportGenerationPrepared = {
      contextDigest: admission.contextDigest,
      policy: admission.policy,
      policyDigest: await transportAuthorizationDigestWasm(admission.policy),
      connect: {
        servers: options.servers,
        authenticators: Array.isArray(authOptions.authenticator)
          ? authOptions.authenticator
          : [authOptions.authenticator],
        inboxPrefix,
      },
    };
    await manager.initialize(nc, prepared);
    const abort = () => void manager.close();
    options.signal?.addEventListener("abort", abort, { once: true });
    void (async () => {
      for await (const status of manager.status()) {
        const type = (status as { type?: string }).type;
        if (type === "disconnect") {
          if (renewalTimer !== undefined) clearTimeout(renewalTimer);
          renewalTimer = undefined;
          renewalAbort?.abort();
          owner.setTransportAvailable(false);
        }
        if (type === "reconnect") {
          owner.setTransportAvailable(true);
          scheduleRenewal();
        }
      }
    })();
    void manager.closed().finally(() => {
      owner.stop();
      if (renewalTimer !== undefined) clearTimeout(renewalTimer);
      renewalAbort?.abort();
      options.signal?.removeEventListener("abort", abort);
      unsubscribeOwner();
    });
    const unsubscribeOwner = owner.subscribeChanges(() => {
      if (manager.isClosed()) return;
      if (owner.coverageLost) {
        if (renewalTimer !== undefined) clearTimeout(renewalTimer);
        renewalTimer = undefined;
        renewalAbort?.abort();
        void manager.terminate(
          new TransportError({
            code: "trellis.transport.unavailable",
            message: "Authority revocation coverage is unavailable.",
            hint: "Reconnect when the runtime is available.",
          }),
        );
      } else if (owner.ownRevoked) {
        if (renewalTimer !== undefined) clearTimeout(renewalTimer);
        renewalTimer = undefined;
        renewalAbort?.abort();
        try {
          void manager.currentNats().close();
        } catch {
          /* replacement already owns recovery */
        }
      }
    });
    return {
      manager,
      owner,
      auth: options.auth,
      inboxPrefix,
      refreshAuthority,
      get context() {
        if (!installed) throw new Error("AdmissionUnavailable");
        return installed;
      },
    };
  } catch (error) {
    owner.stop();
    await manager.close();
    await nc?.close();
    throw error;
  }
}
