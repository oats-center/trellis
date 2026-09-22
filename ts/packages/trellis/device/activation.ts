import { mkdir, readFile, rename, unlink, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { env, platform } from "node:process";
import type { StaticDecode } from "typebox";
import { Type } from "typebox";
import { Value } from "typebox/value";
import { ulid } from "ulid";

import { deriveDeviceConfirmationCode, deriveDeviceIdentity } from "../auth.ts";
import type {
  TrellisDeviceActivatedActivationState,
  TrellisDeviceActivationArgs,
  TrellisDeviceLocalActivationState,
  TrellisDevicePendingActivationState,
} from "../device.ts";
import {
  resumeDeviceActivationWithDeps,
  startDeviceActivationWithDeps,
} from "../device.ts";
import { TransportError } from "../errors/index.ts";
import { base64urlDecode, base64urlEncode } from "../auth/utils.ts";

const PendingActivationStateSchema = Type.Object({
  status: Type.Literal("pending"),
  participantId: Type.String({ minLength: 1 }),
  publicIdentityKey: Type.String({ minLength: 1 }),
  instanceId: Type.String({ minLength: 1 }),
  deploymentId: Type.String({ minLength: 1 }),
  flowId: Type.String({ minLength: 1 }),
  nonce: Type.String({ minLength: 1 }),
  activationUrl: Type.String({ minLength: 1 }),
});

const ActivatedActivationStateSchema = Type.Object({
  status: Type.Literal("activated"),
  participantId: Type.String({ minLength: 1 }),
  publicIdentityKey: Type.String({ minLength: 1 }),
  instanceId: Type.String({ minLength: 1 }),
  deploymentId: Type.String({ minLength: 1 }),
  flowId: Type.String({ minLength: 1 }),
  nonce: Type.String({ minLength: 1 }),
  activationUrl: Type.String({ minLength: 1 }),
});

const LocalActivationStateSchema = Type.Union([
  PendingActivationStateSchema,
  ActivatedActivationStateSchema,
]);

const PersistedActivationStateSchema = Type.Object({
  version: Type.Literal(1),
  localState: LocalActivationStateSchema,
});

type PersistedActivationState = StaticDecode<
  typeof PersistedActivationStateSchema
>;

type DeviceActivationStateStoreOptions = {
  trellisUrl: string;
  rootSecret: Uint8Array | string;
  participantId: string;
  stateDir?: string;
  statePath?: string;
};

type DeviceActivationStateStore = {
  readonly statePath: string;
  load(): Promise<TrellisDeviceLocalActivationState | null>;
  save(state: TrellisDeviceLocalActivationState): Promise<void>;
};

/**
 * Options for the native device activation status helper.
 */
export type CheckDeviceActivationArgs<
  TContract extends TrellisDeviceActivationArgs["participant"] =
    TrellisDeviceActivationArgs["participant"],
> = TrellisDeviceActivationArgs<TContract> & {
  stateDir?: string;
  statePath?: string;
};

/**
 * Activation status for a device that is already ready to connect.
 */
export type TrellisDeviceActivatedStatus = {
  status: "activated";
};

/**
 * Activation status for a device that cannot proceed yet.
 */
export type TrellisDeviceNotReadyStatus = {
  status: "not_ready";
  reason: string;
};

/**
 * Activation status for a device that still needs activation.
 */
export type TrellisDeviceActivationRequiredStatus = {
  status: "activation_required";
  activationUrl: string;
  /** Device-local code the activating user must confirm in the portal. */
  confirmationCode: string;
  waitForOnlineApproval(
    opts?: { signal?: AbortSignal },
  ): Promise<TrellisDeviceActivatedStatus>;
  acceptConfirmationCode(code: string): Promise<TrellisDeviceActivatedStatus>;
};

/**
 * Caller-facing activation status union for native device runtimes.
 */
export type TrellisDeviceActivationStatus =
  | TrellisDeviceActivatedStatus
  | TrellisDeviceNotReadyStatus
  | TrellisDeviceActivationRequiredStatus;

function normalizeRootSecret(rootSecret: Uint8Array | string): Uint8Array {
  if (typeof rootSecret === "string") {
    const decoded = base64urlDecode(rootSecret.trim());
    if (decoded.length === 0) {
      throw new Error("rootSecret must not be empty");
    }
    return decoded;
  }

  if (rootSecret.length === 0) {
    throw new Error("rootSecret must not be empty");
  }

  return rootSecret;
}

function isNotFoundError(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

function isAlreadyExistsError(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "EEXIST";
}

function tempStatePath(statePath: string): string {
  return `${statePath}.tmp-${ulid()}`;
}

function backupStatePath(statePath: string): string {
  return `${statePath}.bak`;
}

function defaultActivationStateDir(): string {
  if (platform === "win32") {
    const base = env.LOCALAPPDATA ?? env.APPDATA;
    if (!base) {
      throw new Error(
        "Could not resolve a default Trellis device state directory: LOCALAPPDATA or APPDATA is not set.",
      );
    }
    return join(base, "Trellis", "device-activation");
  }

  const home = env.HOME;
  if (!home) {
    throw new Error(
      "Could not resolve a default Trellis device state directory: HOME is not set.",
    );
  }

  if (platform === "darwin") {
    return join(
      home,
      "Library",
      "Application Support",
      "Trellis",
      "device-activation",
    );
  }

  return join(
    env.XDG_STATE_HOME ?? join(home, ".local", "state"),
    "trellis",
    "device-activation",
  );
}

async function hashOrigin(origin: string): Promise<string> {
  const digest = await crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(origin),
  );
  return base64urlEncode(new Uint8Array(digest));
}

async function deriveStoreIdentity(
  args: DeviceActivationStateStoreOptions,
): Promise<{ publicIdentityKey: string }> {
  const identity = await deriveDeviceIdentity(
    normalizeRootSecret(args.rootSecret),
  );
  return {
    publicIdentityKey: identity.publicIdentityKey,
  };
}

function parsePersistedState(text: string): TrellisDeviceLocalActivationState {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (cause) {
    throw new Error(
      "Stored Trellis device activation state is not valid JSON.",
      {
        cause,
      },
    );
  }

  if (!Value.Check(PersistedActivationStateSchema, parsed)) {
    throw new Error(
      "Stored Trellis device activation state has an invalid shape.",
    );
  }

  const persisted = Value.Parse(
    PersistedActivationStateSchema,
    parsed,
  ) as PersistedActivationState;
  return persisted.localState;
}

function assertLocalStateMatchesIdentity(args: {
  state: TrellisDeviceLocalActivationState;
  publicIdentityKey: string;
}): void {
  if (args.state.publicIdentityKey !== args.publicIdentityKey) {
    throw new Error(
      "Stored Trellis device activation state does not match the provided root secret.",
    );
  }
}

function assertLocalStateMatchesContract(args: {
  state: TrellisDeviceLocalActivationState;
  participantId: string;
}): void {
  if (args.state.participantId !== args.participantId) {
    throw new Error(
      "Stored Trellis device activation state belongs to another participant.",
    );
  }
}

async function resolveDeviceActivationStatePath(
  args: DeviceActivationStateStoreOptions,
): Promise<string> {
  if (args.statePath) {
    return args.statePath;
  }

  const { publicIdentityKey } = await deriveStoreIdentity(args);
  const origin = new URL(args.trellisUrl).origin;
  const originHash = await hashOrigin(origin);
  const fileName = `activation-state-v1-${originHash}-${
    encodeURIComponent(args.participantId)
  }-${publicIdentityKey}.json`;

  return join(args.stateDir ?? defaultActivationStateDir(), fileName);
}

async function openDeviceActivationStateStore(
  args: DeviceActivationStateStoreOptions,
): Promise<DeviceActivationStateStore> {
  const { publicIdentityKey } = await deriveStoreIdentity(args);
  const statePath = await resolveDeviceActivationStatePath(args);
  const stateBackupPath = backupStatePath(statePath);

  return {
    statePath,
    async load(): Promise<TrellisDeviceLocalActivationState | null> {
      for (const path of [statePath, stateBackupPath]) {
        try {
          const text = new TextDecoder().decode(await readFile(path));
          const state = parsePersistedState(text);
          assertLocalStateMatchesIdentity({ state, publicIdentityKey });
          assertLocalStateMatchesContract({
            state,
            participantId: args.participantId,
          });
          return state;
        } catch (error) {
          if (isNotFoundError(error)) {
            continue;
          }
          throw error;
        }
      }
      return null;
    },
    async save(state: TrellisDeviceLocalActivationState): Promise<void> {
      if (!Value.Check(LocalActivationStateSchema, state)) {
        throw new Error(
          "Trellis device activation state has an invalid shape.",
        );
      }

      assertLocalStateMatchesIdentity({ state, publicIdentityKey });
      assertLocalStateMatchesContract({
        state,
        participantId: args.participantId,
      });
      await mkdir(dirname(statePath), { recursive: true });
      const nextText =
        JSON.stringify({ version: 1, localState: state }, null, 2) +
        "\n";
      const nextPath = tempStatePath(statePath);
      try {
        await writeFile(nextPath, nextText, "utf8");
        try {
          await rename(nextPath, statePath);
        } catch (error) {
          if (!isAlreadyExistsError(error)) {
            throw error;
          }
          try {
            await unlink(stateBackupPath);
          } catch (backupError) {
            if (!isNotFoundError(backupError)) {
              throw backupError;
            }
          }
          await rename(statePath, stateBackupPath);
          try {
            await rename(nextPath, statePath);
            await unlink(stateBackupPath);
          } catch (renameError) {
            try {
              await rename(stateBackupPath, statePath);
            } catch (rollbackError) {
              if (!isNotFoundError(rollbackError)) {
                throw rollbackError;
              }
            }
            throw renameError;
          }
        }
      } catch (error) {
        try {
          await unlink(nextPath);
        } catch (cleanupError) {
          if (!isNotFoundError(cleanupError)) {
            throw cleanupError;
          }
        }
        throw error;
      }
    },
  };
}

function activatedStatus(): TrellisDeviceActivatedStatus {
  return { status: "activated" };
}

function createActivatedLocalState(
  localState:
    | TrellisDevicePendingActivationState
    | TrellisDeviceActivatedActivationState,
): TrellisDeviceActivatedActivationState {
  return {
    ...localState,
    status: "activated",
  };
}

async function createActivationRequiredStatus<
  TContract extends TrellisDeviceActivationArgs["participant"],
>(args: {
  checkArgs: CheckDeviceActivationArgs<TContract>;
  store: DeviceActivationStateStore;
  localState: TrellisDeviceLocalActivationState | null;
}): Promise<TrellisDeviceActivationRequiredStatus> {
  const session = args.localState?.status === "pending"
    ? await resumeDeviceActivationWithDeps({
      trellisUrl: args.checkArgs.trellisUrl,
      participant: args.checkArgs.participant,
      provisioningSecret: args.checkArgs.provisioningSecret,
      rootSecret: args.checkArgs.rootSecret,
      localState: args.localState,
    }, { now: () => Date.now() })
    : await startDeviceActivationWithDeps({
      trellisUrl: args.checkArgs.trellisUrl,
      participant: args.checkArgs.participant,
      provisioningSecret: args.checkArgs.provisioningSecret,
      rootSecret: args.checkArgs.rootSecret,
    }, { now: () => Date.now() });

  if (args.localState?.status !== "pending") {
    await args.store.save(session.localState);
  }

  let completedState: TrellisDeviceActivatedActivationState | null = null;
  const identity = await deriveDeviceIdentity(
    normalizeRootSecret(args.checkArgs.rootSecret),
  );
  const confirmationCode = await deriveDeviceConfirmationCode({
    activationKey: identity.activationKey,
    publicIdentityKey: identity.publicIdentityKey,
    nonce: session.localState.nonce,
  });
  let completionPromise: Promise<TrellisDeviceActivatedStatus> | null = null;
  const finish = async (
    nextState: () => Promise<TrellisDeviceActivatedActivationState>,
  ): Promise<TrellisDeviceActivatedStatus> => {
    if (completedState) {
      return activatedStatus();
    }

    if (completionPromise) {
      return await completionPromise;
    }

    completionPromise = (async () => {
      const nextCompletedState = await nextState();
      await args.store.save(nextCompletedState);
      completedState = nextCompletedState;
      return activatedStatus();
    })();
    try {
      return await completionPromise;
    } catch (error) {
      completionPromise = null;
      throw error;
    }
  };

  return {
    status: "activation_required",
    activationUrl: session.activationUrl,
    confirmationCode,
    waitForOnlineApproval(opts?: { signal?: AbortSignal }) {
      return finish(() => session.waitForOnlineApproval(opts));
    },
    acceptConfirmationCode(code: string) {
      return finish(() => session.acceptConfirmationCode(code));
    },
  };
}

/**
 * Reports native device activation status and hides local activation persistence details.
 */
export async function checkDeviceActivation<
  TContract extends TrellisDeviceActivationArgs["participant"],
>(
  args: CheckDeviceActivationArgs<TContract>,
): Promise<TrellisDeviceActivationStatus> {
  const store = await openDeviceActivationStateStore({
    trellisUrl: args.trellisUrl,
    rootSecret: args.rootSecret,
    participantId: args.participant.identity,
    stateDir: args.stateDir,
    statePath: args.statePath,
  });
  const localState = await store.load();
  try {
    return await createActivationRequiredStatus({
      checkArgs: args,
      store,
      localState,
    });
  } catch (error) {
    if (
      error instanceof TransportError &&
      error.code === "device_activation_unavailable" &&
      error.getContext().status === "ready"
    ) {
      if (localState?.status === "pending") {
        await store.save(createActivatedLocalState(localState));
      }
      return activatedStatus();
    }
    if (
      error instanceof TransportError &&
      error.code === "device_activation_unavailable" &&
      error.getContext().status === "not_ready"
    ) {
      return { status: "not_ready", reason: "activation_rejected" };
    }
    throw error;
  }
}
