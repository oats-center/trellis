import type { NatsConnection } from "@nats-io/nats-core";
export { checkDeviceActivation } from "./device/activation.ts";
export type {
  CheckDeviceActivationArgs,
  TrellisDeviceActivatedStatus,
  TrellisDeviceActivationRequiredStatus,
  TrellisDeviceActivationStatus,
  TrellisDeviceNotReadyStatus,
} from "./device/activation.ts";
import { AsyncResult, Result, UnexpectedError } from "@oatscenter/result";
import { ulid } from "ulid";
import {
  type ProvisionedConnectionOptions,
  connectProvisioned,
} from "./auth/connection.ts";
import {
  connectNativeClient,
  type ConnectedNativeClient,
} from "./native/connection.ts";
import type { NativeOAuthSession } from "./native/oauth.ts";
import { ContractResourceBindingsSchema } from "./participant.ts";

import {
  deriveDeviceConfirmationCode,
  deriveDeviceIdentity,
  requestDeviceEnrollment,
  verifyDeviceConfirmationCode,
  waitForDeviceActivation,
} from "./auth/device_activation.ts";
import {
  base64urlDecode,
  base64urlEncode,
  sha256,
  utf8,
} from "./auth/utils.ts";
import { createAuth } from "./auth/session_auth.ts";
import type { RuntimeApi } from "./participant_runtime/api.ts";
import { loadDefaultRuntimeTransport } from "./runtime_transport.ts";
import {
  type ServiceHealth,
  ServiceHealthRuntime,
} from "./service/runtime/health.ts";
import { createHealthHeartbeatPublisher } from "./health_transport.ts";
import { Trellis } from "./session.ts";
import { logger as noopLogger, type LoggerLike } from "./globals.ts";
import { TransportError } from "./errors/index.ts";
import { Type } from "typebox";
import { Value } from "typebox/value";
import {
  observeTrellisConnection,
  transitionConnectionAvailability,
} from "./connection.ts";
import { type CallerRuntime, createCallerRuntime } from "./caller.ts";
import {
  type GeneratedParticipant,
  getParticipantRuntime,
  participantEvidence,
} from "./participant_runtime/participant.ts";
import type { AppRequestDescriptor } from "./generated_support.ts";

type DeviceContract = GeneratedParticipant;

function deviceConnectResult<T>(
  promise: Promise<T>,
): AsyncResult<T, TransportError | UnexpectedError> {
  return AsyncResult.from(
    promise.then(
      (value): Result<T, TransportError | UnexpectedError> => Result.ok(value),
      (cause): Result<T, TransportError | UnexpectedError> =>
        Result.err(
          cause instanceof TransportError
            ? cause
            : new UnexpectedError({ cause }),
        ),
    ),
  );
}

export type TrellisDeviceConnection<
  TContract extends DeviceContract = DeviceContract,
> = CallerRuntime<TContract> & {
  readonly health: ServiceHealth;
} & (TContract extends {
    companion: {
      request: infer TChild extends AppRequestDescriptor;
      availability: infer TAvailability;
    };
  }
    ? {
        readonly companion: TAvailability extends "required"
          ? ConnectedNativeClient<TChild>
          : ConnectedNativeClient<TChild> | undefined;
      }
    : { readonly companion?: never });

type DeviceConnectTransport = {
  connect(options: {
    servers: string | string[];
    token?: string;
    authenticator?: unknown;
    inboxPrefix?: string;
    maxReconnectAttempts?: number;
    ignoreAuthErrorAbort?: boolean;
    timeout?: number;
  }): Promise<NatsConnection>;
};

type DeviceConnectDeps = {
  loadTransport(): Promise<DeviceConnectTransport>;
  now(): number;
};

export type TrellisDevicePendingActivationState = {
  status: "pending";
  participantId: string;
  publicIdentityKey: string;
  instanceId: string;
  deploymentId: string;
  flowId: string;
  nonce: string;
  activationUrl: string;
};

export type TrellisDeviceActivatedActivationState = {
  status: "activated";
  participantId: string;
  publicIdentityKey: string;
  instanceId: string;
  deploymentId: string;
  flowId: string;
  nonce: string;
  activationUrl: string;
};

export type TrellisDeviceLocalActivationState =
  TrellisDevicePendingActivationState | TrellisDeviceActivatedActivationState;

export type TrellisDeviceActivationSession<
  TState extends TrellisDeviceLocalActivationState =
    TrellisDeviceLocalActivationState,
> = {
  activationUrl: string;
  localState: TState;
  waitForOnlineApproval(opts?: {
    signal?: AbortSignal;
  }): Promise<TrellisDeviceActivatedActivationState>;
  acceptConfirmationCode(
    code: string,
  ): Promise<TrellisDeviceActivatedActivationState>;
};

export type TrellisDeviceActivationArgs<
  TContract extends DeviceContract = DeviceContract,
> = {
  trellisUrl: string;
  participant: TContract;
  rootSecret: Uint8Array | string;
  provisioningSecret?: string;
};

/** Provisioned device identity and exact deployment participant binding. */
export type TrellisDeviceProvisionedIdentity = {
  deploymentId: string;
  instanceId: string;
  principalId: string;
  participantId: string;
  participantDigest: string;
  participantNeedsDigest: string;
  provisioningSecret?: string;
  expectedSecretVersion?: number;
};

export type TrellisDeviceResumeActivationArgs<
  TContract extends DeviceContract = DeviceContract,
> = TrellisDeviceActivationArgs<TContract> & {
  localState: TrellisDeviceLocalActivationState;
};

export type TrellisDeviceConnectArgs<
  TContract extends DeviceContract = DeviceContract,
> = ProvisionedConnectionOptions & {
  participant: TContract;
  log?: LoggerLike | false;
  /** Independently consented native OAuth custody for the assigned companion. */
  companion?: { session: NativeOAuthSession; servers: readonly string[] };
};

const DeviceSignedAuthorizationSchema = Type.Object({
  participantId: Type.String(),
  participantDigest: Type.String(),
  resourceRuntime: ContractResourceBindingsSchema,
  companion: Type.Optional(
    Type.Union([
      Type.Null(),
      Type.Object({
        requestId: Type.String(),
        oauthGrantId: Type.String(),
        clientId: Type.String(),
        required: Type.Boolean(),
      }),
    ]),
  ),
});

function normalizeRootSecret(rootSecret: Uint8Array | string): Uint8Array {
  if (typeof rootSecret === "string") {
    const decoded = base64urlDecode(rootSecret.trim());
    if (decoded.length === 0) throw new Error("rootSecret must not be empty");
    return decoded;
  }
  if (rootSecret.length === 0) throw new Error("rootSecret must not be empty");
  return rootSecret;
}

const defaultDeps: DeviceConnectDeps = {
  loadTransport: loadDefaultRuntimeTransport,
  now: () => Date.now(),
};

function transportCauseContext(cause: unknown): Record<string, unknown> {
  if (cause instanceof Error) {
    return { causeName: cause.name, causeMessage: cause.message };
  }

  return { cause: String(cause) };
}

function createTransportError(args: {
  code: string;
  message: string;
  hint: string;
  context?: Record<string, unknown>;
  cause?: unknown;
}): TransportError {
  return new TransportError({
    code: args.code,
    message: args.message,
    hint: args.hint,
    cause: args.cause,
    context: {
      ...(args.context ?? {}),
      ...(args.cause === undefined ? {} : transportCauseContext(args.cause)),
    },
  });
}

function resolveDeviceLogger(log?: LoggerLike | false): LoggerLike {
  if (log === false) {
    return noopLogger;
  }

  return log ?? noopLogger;
}

function createInvalidConfirmationCodeTransportError(
  context?: Record<string, unknown>,
) {
  return createTransportError({
    code: "trellis.device.invalid_confirmation_code",
    message: "The device confirmation code is invalid.",
    hint: "Retry with the current confirmation code for this activation, or restart activation if the code is no longer valid.",
    context,
  });
}

function createActivatedLocalState(
  localState: TrellisDeviceLocalActivationState,
): TrellisDeviceActivatedActivationState {
  return {
    ...localState,
    status: "activated",
  };
}

function assertActivationStateMatchesIdentity(args: {
  localState: TrellisDeviceLocalActivationState;
  publicIdentityKey: string;
}): void {
  if (args.localState.publicIdentityKey !== args.publicIdentityKey) {
    throw createTransportError({
      code: "trellis.device.activation_state_mismatch",
      message:
        "Local device activation state does not match the provided root secret.",
      hint: "Use the activation state for the same device identity, or start a new activation for this root secret.",
      context: {
        statePublicIdentityKey: args.localState.publicIdentityKey,
        publicIdentityKey: args.publicIdentityKey,
      },
    });
  }
}

function assertActivationStateMatchesContract(args: {
  localState: TrellisDeviceLocalActivationState;
  participantId: string;
}): void {
  if (args.localState.participantId !== args.participantId) {
    throw createTransportError({
      code: "trellis.device.activation_state_contract_mismatch",
      message:
        "Local device activation state does not match the requested device participant.",
      hint: "Use activation state for the same device contract, or start activation again for this contract digest.",
      context: {
        stateParticipantId: args.localState.participantId,
        participantId: args.participantId,
      },
    });
  }
}

function createActivationSession<
  TLocalState extends TrellisDeviceLocalActivationState,
>(args: {
  trellisUrl: string;
  participantId: string;
  identity: Awaited<ReturnType<typeof deriveDeviceIdentity>>;
  provisioned: Pick<
    TrellisDeviceProvisionedIdentity,
    "participantId" | "provisioningSecret"
  >;
  participant: DeviceContract;
  now: () => number;
  localState: TLocalState;
  sessionIdentity?: Awaited<ReturnType<typeof createAuth>>;
  connectionId?: string;
}): TrellisDeviceActivationSession<TLocalState> {
  assertActivationStateMatchesIdentity({
    localState: args.localState,
    publicIdentityKey: args.identity.publicIdentityKey,
  });
  assertActivationStateMatchesContract({
    localState: args.localState,
    participantId: args.participantId,
  });

  const activatedState = createActivatedLocalState(args.localState);
  return {
    activationUrl: args.localState.activationUrl,
    localState: args.localState,
    waitForOnlineApproval: async (opts?: { signal?: AbortSignal }) => {
      if (args.localState.status === "activated") {
        return activatedState;
      }

      await waitForDeviceActivation({
        trellisUrl: args.trellisUrl,
        publicIdentityKey: args.identity.publicIdentityKey,
        identitySeed: args.identity.identitySeed,
        activationKey: args.identity.activationKey,
        participantId: args.provisioned.participantId,
        ...participantEvidence(args.participant),
        provisioningSecret: args.provisioned.provisioningSecret ?? undefined,
        nonce: args.localState.nonce,
        signal: opts?.signal,
        sessionIdentity: args.sessionIdentity,
        connectionId: args.connectionId,
      });
      return activatedState;
    },
    acceptConfirmationCode: async (code: string) => {
      if (args.localState.status === "activated") {
        return activatedState;
      }

      const ok = await verifyDeviceConfirmationCode({
        activationKey: args.identity.activationKey,
        publicIdentityKey: args.identity.publicIdentityKey,
        nonce: args.localState.nonce,
        confirmationCode: code,
      });
      if (!ok) {
        throw createInvalidConfirmationCodeTransportError({
          publicIdentityKey: args.identity.publicIdentityKey,
          instanceId: args.localState.instanceId,
          deploymentId: args.localState.deploymentId,
        });
      }
      const enrollment = await requestDeviceEnrollment({
        trellisUrl: args.trellisUrl,
        publicIdentityKey: args.identity.publicIdentityKey,
        identitySeed: args.identity.identitySeed,
        sessionIdentity:
          args.sessionIdentity ??
          (await createAuth({
            sessionKeySeed: base64urlEncode(
              crypto.getRandomValues(new Uint8Array(32)),
            ),
          })),
        connectionId: args.connectionId ?? ulid(),
        participantId: args.participantId,
        ...participantEvidence(args.participant),
        challengeDigest: base64urlEncode(
          await sha256(utf8(args.localState.nonce)),
        ),
        confirmationCode: code,
      });
      if (enrollment.state !== "approved") {
        throw createTransportError({
          code: "device_activation_unavailable",
          message: "The server has not approved this device enrollment.",
          hint: "Complete the administrator review before connecting the device.",
          context: { status: enrollment.state },
        });
      }
      return activatedState;
    },
  };
}

/**
 * @internal Exported for focused tests and platform-specific wrappers.
 */
export async function startDeviceActivationWithDeps<
  TContract extends DeviceContract,
>(
  args: TrellisDeviceActivationArgs<TContract>,
  deps: Pick<DeviceConnectDeps, "now">,
): Promise<
  TrellisDeviceActivationSession<TrellisDevicePendingActivationState>
> {
  const rootSecret = normalizeRootSecret(args.rootSecret);
  const identity = await deriveDeviceIdentity(rootSecret);
  const nonce = base64urlEncode(crypto.getRandomValues(new Uint8Array(32)));
  const sessionIdentity = await createAuth({
    sessionKeySeed: base64urlEncode(crypto.getRandomValues(new Uint8Array(32))),
  });
  const connectionId = ulid();
  const activation = await requestDeviceEnrollment({
    trellisUrl: args.trellisUrl,
    publicIdentityKey: identity.publicIdentityKey,
    identitySeed: identity.identitySeed,
    sessionIdentity,
    connectionId,
    participantId: args.participant.identity,
    ...participantEvidence(args.participant),
    provisioningSecret: args.provisioningSecret,
    challengeDigest: base64urlEncode(await sha256(utf8(nonce))),
    confirmationCode: await deriveDeviceConfirmationCode({
      activationKey: identity.activationKey,
      publicIdentityKey: identity.publicIdentityKey,
      nonce,
    }),
  });
  const activationState = activation.activation as Record<
    string,
    unknown
  > | null;
  if (
    activation.state !== "pending" ||
    typeof activationState?.reviewId !== "string" ||
    typeof activationState.activationUrl !== "string" ||
    typeof activationState.principalId !== "string" ||
    typeof activationState.deploymentId !== "string" ||
    typeof activationState.instanceId !== "string"
  ) {
    throw createTransportError({
      code: "device_activation_unavailable",
      message: "The device does not require activation.",
      hint: "Connect the device directly.",
      context: {
        status: activation.state === "approved" ? "ready" : activation.state,
      },
    });
  }
  return await createActivationSession({
    trellisUrl: args.trellisUrl,
    participantId: args.participant.identity,
    identity,
    provisioned: {
      participantId: args.participant.identity,
      provisioningSecret: args.provisioningSecret,
    },
    participant: args.participant,
    now: deps.now,
    localState: {
      status: "pending",
      participantId: args.participant.identity,
      publicIdentityKey: identity.publicIdentityKey,
      instanceId: activationState.instanceId,
      deploymentId: activationState.deploymentId,
      flowId: activationState.reviewId,
      nonce,
      activationUrl: activationState.activationUrl,
    },
    sessionIdentity,
    connectionId,
  });
}

/**
 * @internal Exported for focused tests and platform-specific wrappers.
 */
export async function resumeDeviceActivationWithDeps<
  TLocalState extends TrellisDeviceLocalActivationState,
  TContract extends DeviceContract,
>(
  args: TrellisDeviceResumeActivationArgs<TContract> & {
    localState: TLocalState;
  },
  deps: Pick<DeviceConnectDeps, "now">,
): Promise<TrellisDeviceActivationSession<TLocalState>> {
  const rootSecret = normalizeRootSecret(args.rootSecret);
  const identity = await deriveDeviceIdentity(rootSecret);

  return await createActivationSession({
    trellisUrl: args.trellisUrl,
    participantId: args.participant.identity,
    identity,
    provisioned: { participantId: args.participant.identity },
    participant: args.participant,
    now: deps.now,
    localState: args.localState,
  });
}

/**
 * @internal Exported for focused tests; applications should use
 * `TrellisDevice.connect`.
 */
export async function connectDeviceWithDeps<TContract extends DeviceContract>(
  args: TrellisDeviceConnectArgs<TContract>,
  deps: DeviceConnectDeps,
): Promise<TrellisDeviceConnection<TContract>> {
  const connected = await connectProvisioned(
    args,
    "device",
    {},
    await deps.loadTransport(),
  );
  let companion: ConnectedNativeClient<AppRequestDescriptor> | undefined;
  try {
    const own = connected.owner.own();
    const binding = own.authority.binding;
    own.release();
    if (binding.kind !== "device") throw new Error("DeviceBindingRequired");
    const signed = Value.Parse(
      DeviceSignedAuthorizationSchema,
      connected.context.registry.authorization,
    );
    if (
      binding.participantId !== args.participant.identity ||
      signed.participantId !== binding.participantId ||
      signed.participantDigest !== args.participant.digest
    )
      throw new Error("DeviceImplementationMismatch");
    const descriptor = args.participant.companion;
    const assignment = signed.companion;
    if (assignment) {
      if (
        !descriptor ||
        descriptor.request.id !== assignment.requestId ||
        descriptor.request.kind !== "native" ||
        (descriptor.availability === "required") !== assignment.required
      )
        throw new Error("CompanionAssignmentMismatch");
      if (args.companion) {
        const session = args.companion.session;
        if (
          session.oauthGrantId !== assignment.oauthGrantId ||
          session.scope.clientId !== assignment.clientId ||
          session.trust.instanceId !== args.trellisInstanceId ||
          session.trust.account !== args.audienceNatsAccount
        )
          throw new Error("CompanionAssignmentMismatch");
        try {
          companion = await connectNativeClient({
            ...args.companion,
            request: descriptor.request,
            timeout: args.timeout,
            log: args.log || undefined,
          }).orThrow();
        } catch (error) {
          if (assignment.required) throw error;
          resolveDeviceLogger(args.log).warn(
            { error },
            "Optional companion unavailable",
          );
        }
      } else if (assignment.required)
        throw new Error("RequiredCompanionUnavailable");
    } else if (descriptor?.availability === "required") {
      throw new Error("RequiredCompanionUnavailable");
    }
    const runtime = getParticipantRuntime(args.participant);
    const manager = connected.manager;
    const connection = observeTrellisConnection({
      kind: "device",
      transport: {
        status: () => manager.status(),
        closed: () => manager.closed(),
        close: () => manager.close(),
        isClosed: () => manager.isClosed(),
        getServer: () => manager.currentNats().getServer(),
      },
      log: args.log ?? false,
    });
    const trellis = new Trellis(
      args.participant.identity,
      manager.currentNats(),
      {
        sessionKey: connected.auth.sessionKey,
        sign: connected.auth.sign,
        contextDigest: () => connected.owner.ownDigest ?? "",
        authorityOwner: connected.owner,
      },
      {
        api: runtime.usedApi as RuntimeApi,
        transport: manager,
        connection,
        log: resolveDeviceLogger(args.log),
        timeout: args.timeout,
        state: runtime.state,
      },
      connected.inboxPrefix,
    );
    const health = new ServiceHealthRuntime({
      serviceName: args.participant.identity,
      kind: "device",
      instanceId: binding.instanceId,
      contractId: binding.participantId,
      contractDigest: signed.participantDigest,
      publishIntervalMs: 30_000,
    });
    const publishHealthSample = createHealthHeartbeatPublisher({
      nc: manager.currentNats(),
      auth: {
        sessionKey: connected.auth.sessionKey,
        sign: connected.auth.sign,
        contextDigest: () => connected.owner.ownDigest ?? "",
        authorityOwner: connected.owner,
      },
      transport: manager,
      inboxPrefix: connected.inboxPrefix,
    });
    const publishHealth = async () => {
      if (manager.isClosed()) return;
      try {
        await publishHealthSample(await health.sample());
      } catch (error) {
        resolveDeviceLogger(args.log).warn(
          { error },
          "Device heartbeat publication failed",
        );
      }
    };
    await publishHealth();
    const heartbeat = setInterval(
      () => void publishHealth(),
      health.publishIntervalMs,
    );
    void manager.closed().finally(() => clearInterval(heartbeat));
    if (companion) {
      const child = companion;
      void manager
        .closed()
        .then(() => child.connection.close())
        .catch(() => undefined);
    }
    transitionConnectionAvailability(connection, true, "connected");
    return Object.assign(createCallerRuntime(trellis, args.participant), {
      health,
      companion,
    }) as TrellisDeviceConnection<TContract>;
  } catch (error) {
    await companion?.connection.close();
    await connected.manager.close();
    throw error;
  }
}

export const TrellisDevice = {
  connect<TContract extends DeviceContract>(
    args: TrellisDeviceConnectArgs<TContract>,
  ): AsyncResult<
    TrellisDeviceConnection<TContract>,
    TransportError | UnexpectedError
  > {
    return deviceConnectResult(connectDeviceWithDeps(args, defaultDeps));
  },
};
