import { AsyncResult } from "@qlever-llc/result";
import type { BaseError } from "@qlever-llc/result";
import type { OperationEvent, OperationSnapshot } from "@qlever-llc/trellis";
import type {
  DeviceUserAuthoritiesResolveOutput,
  DeviceUserAuthoritiesResolveProgress,
} from "@qlever-llc/trellis/auth";
import { assertEquals } from "@std/assert";

import {
  type DeviceActivationAuth,
  type DeviceActivationBindResult,
  type DeviceActivationClient,
  DeviceActivationControllerCore,
  type DeviceActivationOperationRef,
  type DeviceActivationSignInOptions,
} from "./device_activation_controller.ts";

type TestSnapshotOperationRef = DeviceActivationOperationRef & {
  get(): AsyncResult<
    OperationSnapshot<
      DeviceUserAuthoritiesResolveProgress,
      DeviceUserAuthoritiesResolveOutput
    >,
    BaseError
  >;
};

function createStorage(): Pick<Storage, "getItem" | "setItem" | "removeItem"> {
  const values = new Map<string, string>();

  return {
    getItem(key) {
      return values.get(key) ?? null;
    },
    setItem(key, value) {
      values.set(key, value);
    },
    removeItem(key) {
      values.delete(key);
    },
  };
}

function activatedOutput(
  decidedAt = BigInt(Date.parse("2026-04-21T12:34:56Z")),
): DeviceUserAuthoritiesResolveOutput {
  return {
    device: {
      administrativeApproval: "approved",
      createdAt: decidedAt - 1_000n,
      delegationExpiresAt: null,
      delegationRequired: true,
      delegationState: "active",
      deploymentId: "reader.default",
      identityKeyId: null,
      identityPublicKey: null,
      instanceId: "dev_123",
      participantId: null,
      principalId: "device_123",
      state: "active",
      updatedAt: decidedAt,
      version: 2n,
    },
    review: {
      activatedByUserPrincipalId: "usr_123",
      decidedAt,
      decidedBy: "usr_admin",
      deploymentId: "reader.default",
      devicePrincipalId: "device_123",
      expiresAt: decidedAt + 60_000n,
      instanceId: "dev_123",
      reason: null,
      requestedAt: decidedAt - 1_000n,
      reviewId: "dar_123",
      state: "approved",
      version: 2n,
    },
  };
}

function createAuthStub(overrides: {
  handleCallback?: () => Promise<DeviceActivationBindResult | null>;
  signIn?: (options?: DeviceActivationSignInOptions) => Promise<never>;
} = {}): DeviceActivationAuth {
  return {
    init() {
      return Promise.resolve("handle_123");
    },
    handleCallback() {
      return overrides.handleCallback?.() ?? Promise.resolve(null);
    },
    signIn(options?: DeviceActivationSignInOptions) {
      return overrides.signIn?.(options) ??
        Promise.reject(new Error("Redirecting to auth for provider selection"));
    },
  };
}

function createOperationRef(
  output: DeviceUserAuthoritiesResolveOutput,
): DeviceActivationOperationRef {
  const terminal = {
    id: "op_123",
    service: "trellis",
    operation: "Auth.DeviceUserAuthorities.Resolve",
    revision: 2,
    state: "completed" as const,
    createdAt: "2026-04-21T12:00:00Z",
    updatedAt: "2026-04-21T12:00:01Z",
    completedAt: "2026-04-21T12:00:02Z",
    output,
  };

  return {
    wait() {
      return AsyncResult.ok(terminal);
    },
    watch() {
      return AsyncResult.ok((async function* () {
        yield {
          type: "accepted" as const,
          snapshot: {
            ...terminal,
            revision: 1,
            state: "pending" as const,
            output: undefined,
          },
        };
        yield {
          type: "started" as const,
          snapshot: {
            ...terminal,
            revision: 1,
            state: "running" as const,
            output: undefined,
          },
        };
        yield { type: "completed" as const, snapshot: terminal };
      })());
    },
  };
}

function createPendingReviewOperationRef(args: {
  progress: DeviceUserAuthoritiesResolveProgress;
  output: DeviceUserAuthoritiesResolveOutput;
  onProgress(): void;
  waitForCompletion: Promise<void>;
}): TestSnapshotOperationRef {
  const running = {
    id: "op_123",
    service: "trellis",
    operation: "Auth.DeviceUserAuthorities.Resolve",
    revision: 2,
    state: "running" as const,
    createdAt: "2026-04-21T12:00:00Z",
    updatedAt: "2026-04-21T12:00:01Z",
    progress: args.progress,
  };
  const terminal = {
    id: "op_123",
    service: "trellis",
    operation: "Auth.DeviceUserAuthorities.Resolve",
    revision: 3,
    state: "completed" as const,
    createdAt: "2026-04-21T12:00:00Z",
    updatedAt: "2026-04-21T12:00:02Z",
    completedAt: "2026-04-21T12:00:03Z",
    output: args.output,
  };

  return {
    get() {
      return AsyncResult.ok(running);
    },
    wait() {
      return AsyncResult.ok(terminal);
    },
    watch() {
      return AsyncResult.ok((async function* (): AsyncIterable<
        OperationEvent<
          DeviceUserAuthoritiesResolveProgress,
          DeviceUserAuthoritiesResolveOutput
        >
      > {
        yield {
          type: "accepted" as const,
          snapshot: {
            ...running,
            revision: 1,
            state: "pending" as const,
            progress: undefined,
          },
        };
        yield { type: "started" as const, snapshot: running };
        yield {
          type: "progress" as const,
          progress: args.progress,
          snapshot: running,
        };
        args.onProgress();
        await args.waitForCompletion;
        yield { type: "completed" as const, snapshot: terminal };
      })());
    },
  };
}

Deno.test("DeviceActivationController shows sign-in-required before auth", async () => {
  const controller = new DeviceActivationControllerCore({
    authState: createAuthStub(),
    createClient() {
      return Promise.reject(new Error("missing_session_key"));
    },
    getUrl() {
      return new URL(
        "https://auth.example.com/login/device?flowId=device-flow",
      );
    },
    sessionStorage: createStorage(),
  });

  await controller.load();

  assertEquals(controller.view, {
    mode: "sign_in_required",
    flowId: "device-flow",
  });
  assertEquals(controller.authError, null);
});

Deno.test("DeviceActivationController preserves flowId through sign-in callback round-trip", async () => {
  const storage = createStorage();
  let redirectTo: string | undefined;

  const controller = new DeviceActivationControllerCore({
    authState: createAuthStub({
      signIn(options) {
        redirectTo = options?.redirectTo;
        return Promise.reject(
          new Error("Redirecting to auth for provider selection"),
        );
      },
    }),
    createClient() {
      return Promise.reject(new Error("missing_session_key"));
    },
    getUrl() {
      return new URL(
        "https://auth.example.com/login/device?flowId=device-flow#confirm",
      );
    },
    sessionStorage: storage,
    createCallbackToken() {
      return "callback-token";
    },
  });

  await controller.load();
  await controller.signIn();

  assertEquals(
    redirectTo,
    "/login/device?portalCallback=callback-token&deviceFlowId=device-flow#confirm",
  );
  assertEquals(storage.getItem("portal.activate.flowId"), "device-flow");
  assertEquals(
    storage.getItem("portal.activate.callbackToken"),
    "callback-token",
  );
});

Deno.test("DeviceActivationController restores callback flow and maps activation completion", async () => {
  const storage = createStorage();
  storage.setItem("portal.activate.flowId", "device-flow");
  storage.setItem("portal.activate.callbackToken", "callback-token");

  const replacedUrls: string[] = [];
  const authUrlStates: Array<{ currentUrl: URL; redirectTo: string }> = [];
  let activateFlowId: string | null = null;
  const client: DeviceActivationClient = {
    activateDevice(input) {
      activateFlowId = input.flowId;
      return Promise.resolve(createOperationRef(activatedOutput()));
    },
  };

  const controller = new DeviceActivationControllerCore({
    authState: createAuthStub({
      handleCallback() {
        return Promise.resolve({ status: "bound" });
      },
    }),
    createClient(nextAuthUrlState) {
      authUrlStates.push(nextAuthUrlState);
      return Promise.resolve(client);
    },
    getUrl() {
      return new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&deviceFlowId=device-flow&flowId=auth-flow&authError=ignored#confirm",
      );
    },
    replaceUrl(url) {
      replacedUrls.push(url);
    },
    sessionStorage: storage,
  });

  await controller.load();
  assertEquals(controller.view, { mode: "ready", flowId: "device-flow" });
  assertEquals(replacedUrls, [
    "/login/device?flowId=device-flow#confirm",
  ]);
  assertEquals(storage.getItem("portal.activate.flowId"), null);
  assertEquals(storage.getItem("portal.activate.callbackToken"), null);
  assertEquals(
    authUrlStates[0]?.currentUrl.toString(),
    "https://auth.example.com/login/device#confirm",
  );
  assertEquals(
    authUrlStates[0]?.redirectTo,
    "https://auth.example.com/login/device?flowId=device-flow#confirm",
  );

  controller.confirmationCode = "HMHF9MKZ";
  await controller.requestActivation();

  assertEquals(activateFlowId, "device-flow");
  assertEquals(controller.view, {
    mode: "activated",
    flowId: "device-flow",
    instanceId: "dev_123",
    deploymentId: "reader.default",
    activatedAt: "2026-04-21T12:34:56.000Z",
  });
});

Deno.test("DeviceActivationController restores callback flow from URL fallback", async () => {
  const replacedUrls: string[] = [];
  let callbackUrl: string | null = null;

  const controller = new DeviceActivationControllerCore({
    authState: createAuthStub({
      handleCallback() {
        callbackUrl = "called";
        return Promise.resolve({ status: "bound" });
      },
    }),
    createClient() {
      return Promise.resolve({
        activateDevice() {
          throw new Error("not used");
        },
      });
    },
    getUrl() {
      return new URL(
        "https://auth.example.com/login/device?portalCallback=callback-token&deviceFlowId=device-flow&flowId=auth-flow#confirm",
      );
    },
    replaceUrl(url) {
      replacedUrls.push(url);
    },
    sessionStorage: createStorage(),
  });

  await controller.load();

  assertEquals(callbackUrl, "called");
  assertEquals(controller.view, { mode: "ready", flowId: "device-flow" });
  assertEquals(replacedUrls, [
    "/login/device?flowId=device-flow#confirm",
  ]);
});

Deno.test("DeviceActivationController shows pending review from operation progress before terminal completion", async () => {
  let releaseCompletion = () => {};
  const waitForCompletion = new Promise<void>((resolve) => {
    releaseCompletion = resolve;
  });
  let progressSeen = () => {};
  const progressReached = new Promise<void>((resolve) => {
    progressSeen = resolve;
  });

  const controller = new DeviceActivationControllerCore({
    authState: createAuthStub(),
    createClient() {
      return Promise.resolve({
        activateDevice() {
          return Promise.resolve(createPendingReviewOperationRef({
            progress: {
              state: "review_pending",
              retryAfterMs: 1_000n,
            },
            output: activatedOutput(BigInt(Date.parse("2026-04-21T12:00:03Z"))),
            onProgress: progressSeen,
            waitForCompletion,
          }));
        },
      });
    },
    getUrl() {
      return new URL(
        "https://auth.example.com/login/device?flowId=device-flow",
      );
    },
    sessionStorage: createStorage(),
  });

  await controller.load();
  controller.confirmationCode = "HMHF9MKZ";

  const activationRequest = controller.requestActivation();
  await progressReached;
  await Promise.resolve();

  assertEquals(controller.view, {
    mode: "pending_review",
    flowId: "device-flow",
    state: "review_pending",
  });

  releaseCompletion();
  await activationRequest;

  assertEquals(controller.view, {
    mode: "activated",
    flowId: "device-flow",
    instanceId: "dev_123",
    deploymentId: "reader.default",
    activatedAt: "2026-04-21T12:00:03.000Z",
  });
});
