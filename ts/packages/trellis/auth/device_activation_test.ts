import {
  assert,
  assertEquals,
  assertNotEquals,
  assertRejects,
} from "@std/assert";
import { AsyncResult } from "@oats-center/result";
import type {
  OperationEvent,
  OperationSignalAck,
  OperationSnapshot,
  TerminalOperation,
} from "../operations.ts";
import { UnexpectedError } from "../errors/index.ts";

import {
  type AuthDeviceUserAuthoritiesListInput,
  type AuthDeviceUserAuthoritiesListOutput,
  type AuthDeviceUserAuthoritiesRevokeInput,
  type AuthDeviceUserAuthoritiesRevokeResponse,
  type AuthResolveDeviceUserAuthoritiesInput,
  type AuthResolveDeviceUserAuthoritiesOperation,
  type AuthResolveDeviceUserAuthoritiesOutput,
  type AuthResolveDeviceUserAuthoritiesProgress,
  buildDeviceActivationPayload,
  createDeviceActivationClient,
  deriveDeviceConfirmationCode,
  deriveDeviceIdentity,
  deriveDeviceUserCompanion,
  type DeviceActivationTransport,
  encodeDeviceActivationPayload,
  parseDeviceActivationPayload,
  verifyDeviceConfirmationCode,
  waitForDeviceActivation,
} from "./device_activation.ts";

Deno.test("device companion derivation binds canonical origin and exact child", async () => {
  const companion = await deriveDeviceUserCompanion(
    new Uint8Array(32).fill(7),
    "https://example.com/path",
    "acme.Sensor.Companion",
  );
  assertEquals(
    companion.installationSeedBase64url,
    "vIo7nClzQFE0Ir1R7AQz3aqO4NJ5uU2-wXJLaqcyVss",
  );
  assertEquals(
    companion.installationPublicKey,
    "vGf_UmbZSDEhpK6sxpO1esqkNwuof8fwvxvaNG20xZw",
  );
  assertNotEquals(
    companion.installationPublicKey,
    (await deriveDeviceUserCompanion(
      new Uint8Array(32).fill(7),
      "https://other.example.com",
      "acme.Sensor.Companion",
    )).installationPublicKey,
  );
});
const PARTICIPANT_DIGEST = "A".repeat(43);

function unsupportedActivationOperationControl() {
  return AsyncResult.err(
    new UnexpectedError({
      cause: new Error("fake activation operation control is unsupported"),
    }),
  );
}

Deno.test("device activation payload helpers round-trip encoded payloads", async () => {
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(7));
  const payload = await buildDeviceActivationPayload({
    activationKey: identity.activationKey,
    publicIdentityKey: identity.publicIdentityKey,
    nonce: "nonce_123",
  });

  const encoded = encodeDeviceActivationPayload(payload);
  assertEquals(parseDeviceActivationPayload(encoded), payload);
});

Deno.test("device activation helpers verify confirmation codes", async () => {
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(9));

  const confirmationCode = await deriveDeviceConfirmationCode({
    activationKey: identity.activationKey,
    publicIdentityKey: identity.publicIdentityKey,
    nonce: "nonce_123",
  });
  assertEquals(confirmationCode.length, 8);
  assert(
    await verifyDeviceConfirmationCode({
      activationKey: identity.activationKey,
      publicIdentityKey: identity.publicIdentityKey,
      nonce: "nonce_123",
      confirmationCode: confirmationCode.toLowerCase(),
    }),
  );
});

function bootstrapWaitArgs(
  identity: Awaited<ReturnType<typeof deriveDeviceIdentity>>,
) {
  return {
    trellisUrl: "https://trellis.example.com",
    publicIdentityKey: identity.publicIdentityKey,
    identitySeed: identity.identitySeed,
    activationKey: identity.activationKey,
    deploymentId: "reader.default",
    instanceId: "dev_123",
    principalId: "device_123",
    participantId: "acme.reader@v1",
    packageEvidence: {
      rootPackage: "acme",
      rootDigest: PARTICIPANT_DIGEST,
      packages: [{
        name: "acme",
        version: "1.0.0",
        digest: PARTICIPANT_DIGEST,
        source: "package acme@1.0.0;",
      }],
    },
    participantPath: "reader@v1",
    packageDigest: PARTICIPANT_DIGEST,
    nonce: "nonce_123",
  };
}

function activationPendingBody(): Record<string, unknown> {
  const serverNow = Date.now();
  return {
    state: "pending",
    serverNow,
    activation: {
      state: "pending",
      reviewId: "dar_123",
      activationUrl: "https://trellis.example.com/login/device?flowId=dar_123",
      expiresAt: serverNow + 1_000,
      retryAfterMs: 5,
    },
  };
}

function activationApprovedBody(): Record<string, unknown> {
  return {
    state: "approved",
    serverNow: Date.now(),
    activation: null,
  };
}

function bootstrapReadyBody(): Record<string, unknown> {
  return {
    state: "ready",
    serverNow: Date.now(),
  };
}

Deno.test("device activation wait retries the bootstrap route until ready", async () => {
  const originalFetch = globalThis.fetch;
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(5));
  const waitArgs = bootstrapWaitArgs(identity);
  const urls: string[] = [];
  const sessionKeys: string[] = [];
  const requests: Record<string, unknown>[] = [];
  let calls = 0;

  try {
    globalThis.fetch = ((input: URL | Request | string, init?: RequestInit) => {
      urls.push(String(input));
      const request = JSON.parse(String(init?.body)) as Record<string, unknown>;
      requests.push(request);
      sessionKeys.push(
        typeof request.sessionKey === "string" ? request.sessionKey : "",
      );
      calls += 1;
      const body = calls === 1
        ? activationPendingBody()
        : calls === 2
        ? activationApprovedBody()
        : bootstrapReadyBody();
      return Promise.resolve(
        new Response(
          JSON.stringify(body),
          {
            status: 200,
            headers: { "Content-Type": "application/json" },
          },
        ),
      );
    }) as typeof fetch;

    const ready = await waitForDeviceActivation({
      ...waitArgs,
      pollIntervalMs: 0,
    });

    assertEquals(calls, 3);
    assertEquals(urls.map((url) => new URL(url).pathname), [
      "/auth/device/enroll",
      "/auth/device/enroll",
      "/bootstrap/device",
    ]);
    assertEquals(sessionKeys[0], sessionKeys[1]);
    assertEquals(
      requests.map(({ packageEvidence, participantPath, packageDigest }) => ({
        packageEvidence,
        participantPath,
        packageDigest,
      })),
      Array(3).fill({
        packageEvidence: waitArgs.packageEvidence,
        participantPath: waitArgs.participantPath,
        packageDigest: waitArgs.packageDigest,
      }),
    );
    assertEquals(ready.sessionIdentity.sessionKey, sessionKeys[0]);
    assertEquals(ready.bundle.state, "ready");
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("device activation wait honors retryAfterMs from activation_pending", async () => {
  const originalFetch = globalThis.fetch;
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(6));
  const waitArgs = bootstrapWaitArgs(identity);
  let calls = 0;

  try {
    globalThis.fetch =
      ((_input: URL | Request | string, _init?: RequestInit) => {
        calls += 1;
        const body = calls === 1
          ? activationPendingBody()
          : calls === 2
          ? activationApprovedBody()
          : bootstrapReadyBody();
        return Promise.resolve(
          new Response(
            JSON.stringify(body),
            {
              status: 200,
              headers: { "Content-Type": "application/json" },
            },
          ),
        );
      }) as typeof fetch;

    const startedAt = Date.now();
    await waitForDeviceActivation({
      ...waitArgs,
      pollIntervalMs: 0,
    });
    assert(Date.now() - startedAt >= 5);
    assertEquals(calls, 3);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("device activation wait rejects non-success bootstrap responses", async () => {
  const originalFetch = globalThis.fetch;
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(7));
  const waitArgs = bootstrapWaitArgs(identity);

  try {
    globalThis.fetch = ((_input: URL | Request | string, _init?: RequestInit) =>
      Promise.resolve(
        new Response(
          JSON.stringify({
            error: { code: "contract_digest_not_allowed" },
          }),
          {
            status: 403,
            headers: { "Content-Type": "application/json" },
          },
        ),
      )) as typeof fetch;

    await assertRejects(
      () =>
        waitForDeviceActivation({
          ...waitArgs,
          pollIntervalMs: 0,
        }),
      Error,
      "Trellis HTTP 403: contract_digest_not_allowed",
    );
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("device activation wait retries transient fetch failures", async () => {
  const originalFetch = globalThis.fetch;
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(8));
  const waitArgs = bootstrapWaitArgs(identity);
  let calls = 0;

  try {
    globalThis.fetch = ((_: URL | Request | string, _init?: RequestInit) => {
      calls += 1;
      if (calls === 1) {
        return Promise.reject(new TypeError("connection refused"));
      }
      return Promise.resolve(
        new Response(
          JSON.stringify(
            calls === 2 ? activationApprovedBody() : bootstrapReadyBody(),
          ),
          {
            status: 200,
            headers: { "Content-Type": "application/json" },
          },
        ),
      );
    }) as typeof fetch;

    await waitForDeviceActivation({
      ...waitArgs,
      pollIntervalMs: 0,
    });

    assertEquals(calls, 3);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

Deno.test("device activation wait reports an explicit replacement review", async () => {
  const originalFetch = globalThis.fetch;
  const identity = await deriveDeviceIdentity(new Uint8Array(32).fill(10));
  const waitArgs = bootstrapWaitArgs(identity);
  let calls = 0;

  try {
    globalThis.fetch =
      ((_input: URL | Request | string, _init?: RequestInit) => {
        calls += 1;
        const serverNow = Date.now();
        return Promise.resolve(
          new Response(
            JSON.stringify({
              state: "pending",
              serverNow,
              activation: {
                state: "pending",
                reviewId: calls === 1 ? "dar_123" : "dar_replacement",
                activationUrl: "https://trellis.example.com/devices/activate",
                expiresAt: serverNow + 1_000,
                retryAfterMs: 0,
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }) as typeof fetch;

    await assertRejects(
      () => waitForDeviceActivation({ ...waitArgs, pollIntervalMs: 0 }),
      Error,
      "device activation review expired",
    );
    assertEquals(calls, 2);
  } finally {
    globalThis.fetch = originalFetch;
  }
});
