// Console wire-contract regressions. Every assertion goes through the
// generated TypeScript client against the ordinary runtime.

import { assert, assertEquals, assertExists } from "@std/assert";
import { ulid } from "ulid";
import { apis } from "../packages/trellis-test/trellis/index.js";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { TrellisService } from "@oatscenter/trellis/service";
import { withTrellisRuntime } from "./_support/runtime.ts";

// The runtime serializes `ResourceProviderIdentity` with camelCase variant names
// and the enum's own snake_case field names. The IDL declares `providerIdentity`
// as opaque bytes and its unused `AuthResource*Provider` models spell multi-word
// fields in camelCase; the wire payload is authoritative here.
const PROVIDER_JSON_BY_KIND: Record<string, (value: unknown) => boolean> = {
  kv: (value) =>
    isRecord(value) && value.kind === "kv" && typeof value.bucket === "string",
  state: (value) =>
    isRecord(value) && value.kind === "state" &&
    typeof value.bucket === "string",
  store: (value) =>
    isRecord(value) && value.kind === "store" &&
    typeof value.bucket === "string",
  jobQueue: (value) =>
    isRecord(value) && value.kind === "jobQueue" &&
    typeof value.namespace === "string" &&
    typeof value.work_stream === "string" &&
    typeof value.work_subject === "string" &&
    typeof value.consumer === "string",
  eventConsumer: (value) =>
    isRecord(value) && value.kind === "eventConsumer" &&
    typeof value.stream === "string" &&
    typeof value.consumer === "string" &&
    Array.isArray(value.filter_subjects),
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function decodeProviderIdentity(bytes: Uint8Array): unknown {
  return JSON.parse(new TextDecoder().decode(bytes));
}

Deno.test("R01 generated Deployments.Get returns a no-participant profile", async () => {
  await withTrellisRuntime(async (runtime) => {
    const displayName = `ct-r01-${ulid().toLowerCase().slice(-10)}`;
    const created = await runtime.callAdminRpc("authDeploymentsCreate", {
      displayName,
      expiresAt: null,
      idempotencyKey: ulid(),
      kind: "service",
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    });
    const deploymentId = created.deployment.deploymentId;
    assertEquals(created.deployment.participantId, null);

    const detail = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId,
    });

    assertEquals(detail.deployment.deploymentId, deploymentId);
    assertEquals(detail.deployment.displayName, displayName);
    assertEquals(detail.deployment.participantId, null);
    assertEquals(detail.binding, null);
    assertEquals(detail.resources, []);
  });
});

Deno.test("generated Deployments.Create replay returns the original profile", async () => {
  await withTrellisRuntime(async (runtime) => {
    const input = {
      displayName: `ct-replay-${ulid().toLowerCase().slice(-10)}`,
      expiresAt: null,
      idempotencyKey: ulid(),
      kind: "service" as const,
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    };
    const created = await runtime.callAdminRpc("authDeploymentsCreate", input);
    const replayed = await runtime.callAdminRpc("authDeploymentsCreate", input);
    assertEquals(replayed.deployment, created.deployment);
    const persisted = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: created.deployment.deploymentId,
    });
    assertEquals(persisted.deployment, created.deployment);
  });
});

Deno.test("generated portal and route writes replay their original result", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = `ct-replay-${ulid().toLowerCase().slice(-10)}`;
    const portalInput = {
      portalId,
      displayName: "Original portal",
      entryUrl: "https://portal-replay.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    };
    const original = await runtime.callAdminRpc("authPortalsPut", portalInput);
    assertEquals(
      await runtime.callAdminRpc("authPortalsPut", portalInput),
      original,
    );
    const edited = await runtime.callAdminRpc("authPortalsPut", {
      ...portalInput,
      displayName: "Later portal",
      expectedVersion: original.portal.version,
      idempotencyKey: ulid(),
    });
    assertEquals(
      await runtime.callAdminRpc("authPortalsPut", portalInput),
      original,
    );
    const settingsInput = {
      portalId,
      expectedVersion: edited.portal.version,
      idempotencyKey: ulid(),
      settings: {
        localLogin: false,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    };
    const settings = await runtime.callAdminRpc(
      "authPortalsLoginSettingsUpdate",
      settingsInput,
    );
    assertEquals(
      await runtime.callAdminRpc(
        "authPortalsLoginSettingsUpdate",
        settingsInput,
      ),
      settings,
    );

    const routeInput = {
      portalId,
      participantId: null,
      deploymentId: null,
      origin: "https://portal-replay.example.com",
      priority: 10n,
      routeId: null,
      expectedVersion: null,
      idempotencyKey: ulid(),
    };
    const route = await runtime.callAdminRpc(
      "authPortalsRoutesPut",
      routeInput,
    );
    assertEquals(
      await runtime.callAdminRpc("authPortalsRoutesPut", routeInput),
      route,
    );
    const laterRoute = await runtime.callAdminRpc("authPortalsRoutesPut", {
      ...routeInput,
      routeId: route.route.routeId,
      expectedVersion: route.route.version,
      priority: 11n,
      idempotencyKey: ulid(),
    });
    assertEquals(laterRoute.route.version, route.route.version + 1n);
    assertEquals(
      await runtime.callAdminRpc("authPortalsRoutesPut", routeInput),
      route,
    );
    const persisted = await runtime.callAdminRpc("authPortalsGet", {
      portalId,
    });
    assertEquals(persisted.routes.length, 1);
    assertEquals(persisted.routes[0].routeId, route.route.routeId);
    assertEquals(persisted.routes[0].priority, 11n);
    assertEquals(persisted.portal.version, settings.version);
  });
});

Deno.test("generated Users.Create replay returns the original account", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const suffix = ulid().toLowerCase().slice(-10);
    const input = {
      email: `ct-user-replay-${suffix}@example.com`,
      idempotencyKey: ulid(),
      image: null,
      name: "Replay User",
      username: `ct-user-replay-${suffix}`,
    };
    const created = await runtime.callAdminRpc("authUsersCreate", input);
    const replayed = await runtime.callAdminRpc("authUsersCreate", input);
    assertEquals(replayed.user, created.user);
    assertEquals(replayed.user.userId, created.user.userId);
  });
});

Deno.test("V17/N16/R02 generated Deployments.Get returns byte provider identities for all five families", async () => {
  await withTrellisRuntime(async (runtime) => {
    const displayName = `ct-r02-${ulid().toLowerCase().slice(-10)}`;
    await runtime.deployments.create({ id: displayName, kind: "service" });
    const key = await runtime.registerService({
      name: displayName,
      contract: participants.Provider.participant,
      deployment: displayName,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: displayName,
      seed: key.seed,
    }).orThrow();
    try {
      void service.wait().catch(() => {});

      const deadline = Date.now() + 30_000;
      let detail = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: key.deploymentId,
      });
      while (detail.resources.length < 4 && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 250));
        detail = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: key.deploymentId,
        });
      }

      assertExists(detail.binding);
      const kinds = new Set(
        detail.resources.map((resource) => resource.resourceKind),
      );
      // A service deployment cannot own State, so the four service families
      // are asserted here and the State family is proven by the device
      // deployment extension below.
      for (const expected of ["kv", "store", "jobQueue", "eventConsumer"]) {
        assert(
          kinds.has(expected),
          `expected ${expected} evidence, got ${[...kinds]}`,
        );
      }
      for (const resource of detail.resources) {
        assert(
          resource.providerIdentity instanceof Uint8Array,
          `${resource.localName} providerIdentity decoded as ${
            Object.prototype.toString.call(resource.providerIdentity)
          }`,
        );
        const decoded = decodeProviderIdentity(resource.providerIdentity);
        const matches = PROVIDER_JSON_BY_KIND[resource.resourceKind];
        assertExists(
          matches,
          `unexpected resource kind ${resource.resourceKind}`,
        );
        assert(
          matches(decoded),
          `${resource.localName} decoded provider payload ${
            JSON.stringify(decoded)
          }`,
        );
      }
    } finally {
      // Stop the live provider before the runtime dies; otherwise its socket
      // reconnects against a dead server and surfaces an uncaught reset.
      await service.stop();
    }

    const deviceName = `ct-r02-state-${ulid().toLowerCase().slice(-10)}`;
    await runtime.deployments.create({ id: deviceName, kind: "device" });
    await runtime.contracts.install({
      contract: participants.Device.Companion.participant,
    });
    const deviceApplied = await runtime.contracts.apply({
      deployment: deviceName,
      contract: participants.Device.participant,
    });
    const deviceId = deviceApplied.deploymentId!;
    const deviceDeadline = Date.now() + 30_000;
    let deviceDetail = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: deviceId,
    });
    while (
      !deviceDetail.resources.some((resource) =>
        resource.resourceKind === "state"
      ) &&
      Date.now() < deviceDeadline
    ) {
      await new Promise((resolve) => setTimeout(resolve, 250));
      deviceDetail = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: deviceId,
      });
    }
    const stateResource = deviceDetail.resources.find((resource) =>
      resource.resourceKind === "state"
    );
    assertExists(
      stateResource,
      `expected State evidence, got ${
        JSON.stringify(deviceDetail.resources.map((item) => item.resourceKind))
      }`,
    );
    assertEquals(stateResource.localName, "telemetry");
    assert(stateResource.providerIdentity instanceof Uint8Array);
    const decodedState = decodeProviderIdentity(stateResource.providerIdentity);
    assert(
      PROVIDER_JSON_BY_KIND.state(decodedState),
      `State provider payload ${JSON.stringify(decodedState)}`,
    );
  });
});

Deno.test("R10 generated Auth errors decode required fields", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    try {
      await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: `dep_missing_${ulid()}`,
      });
      throw new Error("expected the missing deployment to be rejected");
    } catch (error) {
      const serializable = serializableError(error);
      assertExists(serializable, "expected a serializable Trellis error");
      const decoded = apis.auth.AuthError.fromSerializable(serializable);
      assert(
        decoded.data.code.length > 0,
        "expected a nonempty generated error code",
      );
      assert(
        decoded.data.message.length > 0,
        "expected a nonempty generated error message",
      );
      assert(
        decoded.data.field === null || typeof decoded.data.field === "string",
        "expected field to decode as a nullable string",
      );
      assertEquals(decoded.data.retryable, false);
      assert(
        decoded.data.id.startsWith("err_"),
        `expected an error envelope id, got ${decoded.data.id}`,
      );
    }
  });
});

function serializableError(
  error: unknown,
): Record<string, unknown> | undefined {
  if (isRecord(error) && isRecord(error.remoteError)) {
    return error.remoteError;
  }
  if (
    isRecord(error) && typeof error.toSerializable === "function"
  ) {
    const serialized = (error.toSerializable as () => unknown).call(error);
    return isRecord(serialized) ? serialized : undefined;
  }
  return undefined;
}

Deno.test("R03 capability group edits use returned versions and reject stale", async () => {
  await withTrellisRuntime(async (runtime) => {
    const groupKey = `ct-r03-${ulid().toLowerCase().slice(-10)}`;
    const created = await runtime.callAdminRpc("authCapabilityGroupsPut", {
      groupKey,
      displayName: "R03 Group",
      description: "R03 fixture group",
      capabilities: ["runtime-trellis.r03@v1::alpha"],
      includedGroups: [],
      expectedVersion: null,
      idempotencyKey: ulid(),
    });
    const v1 = created.group.version;
    assert(v1 > 0n, "create returns a bigint version");

    const edited = await runtime.callAdminRpc("authCapabilityGroupsPut", {
      groupKey,
      displayName: "R03 Group edited",
      description: "R03 fixture group",
      capabilities: ["runtime-trellis.r03@v1::beta"],
      includedGroups: [],
      expectedVersion: v1,
      idempotencyKey: ulid(),
    });
    assertEquals(edited.group.version, v1 + 1n);
    assertEquals(edited.group.capabilities, ["runtime-trellis.r03@v1::beta"]);

    // A stale edit under the original version must change nothing.
    const stale = await runtime.callAdminRpc("authCapabilityGroupsPut", {
      groupKey,
      displayName: "R03 stale",
      description: "R03 fixture group",
      capabilities: ["runtime-trellis.r03@v1::stale"],
      includedGroups: [],
      expectedVersion: v1,
      idempotencyKey: ulid(),
    }).then(() => null, (error: unknown) => error);
    assertExists(stale, "the stale edit must be rejected");
    const current = await runtime.callAdminRpc("authCapabilityGroupsGet", {
      groupKey,
    });
    assertEquals(current.group.version, v1 + 1n);
    assertEquals(current.group.displayName, "R03 Group edited");
    assertEquals(current.group.capabilities, ["runtime-trellis.r03@v1::beta"]);
  });
});

Deno.test("R07 session revoke with a stale version is rejected without substituting", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const listed = await runtime.callAdminRpc("authSessionsList", {
      page: { limit: 10 },
    });
    assert(listed.items.length > 0, "at least one real session exists");
    const session = listed.items[0];
    const stale = await runtime.callAdminRpc("authSessionsRevoke", {
      sessionId: session.sessionId,
      expectedVersion: session.version + 99n,
      reason: null,
      idempotencyKey: ulid(),
    }).then(() => null, (error: unknown) => error);
    assertExists(stale, "the stale revoke must be rejected");
    const after = (await runtime.callAdminRpc("authSessionsList", {
      page: { limit: 10 },
    })).items.find((item) => item.sessionId === session.sessionId);
    assertExists(after, "the session still exists");
    assertEquals(
      after.version,
      session.version,
      "a rejected stale revoke must not advance the session version",
    );
    assertEquals(after.state, session.state);
  });
});

Deno.test("R04 portal grant policy edits use versions and a stale replacement is rejected", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participantId = `runtime-trellis.r04-${
      ulid().toLowerCase().slice(-8)
    }@v1`;
    const base = {
      portalId: "builtin",
      participantId,
      directCapabilities: ["runtime-trellis.r04@v1::alpha"],
      capabilityGroupKeys: [],
      roleMappings: [{
        providerId: "github",
        role: "admin",
        directCapabilities: ["runtime-trellis.r04@v1::role"],
        capabilityGroupKeys: [],
      }],
      idempotencyKey: ulid(),
    };
    const created = await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
      ...base,
      expectedVersion: null,
    });
    const v1 = created.policy.version;
    assert(v1 > 0n, "create returns a bigint policy version");
    assertEquals(created.policy.roleMappings.length, 1);

    const edited = await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
      ...base,
      directCapabilities: ["runtime-trellis.r04@v1::beta"],
      expectedVersion: v1,
      idempotencyKey: ulid(),
    });
    assertEquals(edited.policy.version, v1 + 1n);
    assertEquals(edited.policy.directCapabilities, [
      "runtime-trellis.r04@v1::beta",
    ]);
    assertEquals(edited.policy.roleMappings.length, 1);

    // A stale replacement must be rejected and must not drop the roles or
    // capabilities committed by the successful edit.
    const stale = await runtime.callAdminRpc("authPortalsGrantOverridesPut", {
      ...base,
      directCapabilities: ["runtime-trellis.r04@v1::stale"],
      expectedVersion: v1,
      idempotencyKey: ulid(),
    }).then(() => null, (error: unknown) => error);
    assertExists(stale, "the stale replacement must be rejected");
    // A fresh edit under the current version must succeed and preserve the
    // role mapping committed by the earlier successful edit.
    const confirmed = await runtime.callAdminRpc(
      "authPortalsGrantOverridesPut",
      {
        ...base,
        directCapabilities: ["runtime-trellis.r04@v1::gamma"],
        expectedVersion: v1 + 1n,
        idempotencyKey: ulid(),
      },
    );
    assertEquals(confirmed.policy.version, v1 + 2n);
    assertEquals(confirmed.policy.roleMappings.length, 1);
  });
});

Deno.test("R14 a changed payload under the same key is rejected, not replayed", async () => {
  await withTrellisRuntime(async (runtime) => {
    const idempotencyKey = ulid();
    const input = {
      displayName: `ct-r14-${ulid().toLowerCase().slice(-10)}`,
      expiresAt: null,
      idempotencyKey,
      kind: "service" as const,
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    };
    const created = await runtime.callAdminRpc("authDeploymentsCreate", input);
    const changed = await runtime.callAdminRpc("authDeploymentsCreate", {
      ...input,
      displayName: `${input.displayName}-changed`,
    }).then(() => null, (error: unknown) => error);
    assertExists(
      changed,
      "a changed payload under a committed idempotency key must be rejected",
    );
    const persisted = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: created.deployment.deploymentId,
    });
    assertEquals(persisted.deployment.displayName, input.displayName);
  });
});

Deno.test("R05 custom portal round-trips localLogin and providers", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = `ct-r05-${ulid().toLowerCase().slice(-10)}`;
    const created = await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "R05 Portal",
      entryUrl: "https://r05.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: true,
        federatedRegistration: true,
        providers: ["github", "google"],
      },
    });
    assertEquals(created.portal.version, 1n);
    assertEquals(created.portal.loginSettings?.localLogin, true);
    assertEquals(created.portal.loginSettings?.providers, [
      "github",
      "google",
    ]);

    // An explicit list replaces the provider set; null would preserve it.
    const edited = await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "R05 Portal edited",
      entryUrl: "https://r05.example.com",
      disabled: false,
      expectedVersion: created.portal.version,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: false,
        localRegistration: false,
        federatedRegistration: false,
        providers: ["gitlab"],
      },
    });
    assertEquals(edited.portal.version, 2n);
    assertEquals(edited.portal.loginSettings?.localLogin, false);
    assertEquals(edited.portal.loginSettings?.providers, ["gitlab"]);
    assertEquals(edited.portal.loginSettings?.localRegistration, false);

    const reread = await runtime.callAdminRpc("authPortalsGet", { portalId });
    assertEquals(reread.portal.loginSettings?.localLogin, false);
    assertEquals(reread.portal.loginSettings?.providers, ["gitlab"]);

    // The empty list is a real replacement too, not an allow-all shortcut.
    const cleared = await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "R05 Portal edited",
      entryUrl: "https://r05.example.com",
      disabled: false,
      expectedVersion: edited.portal.version,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: false,
        localRegistration: false,
        federatedRegistration: false,
        providers: [],
      },
    });
    assertEquals(cleared.portal.version, 3n);
    assertEquals(cleared.portal.loginSettings?.providers, []);
  });
});

Deno.test("R12 login settings Get-Update-Get matches the envelope and uses its version", async () => {
  await withTrellisRuntime(async (runtime) => {
    const before = await runtime.callAdminRpc("authPortalsLoginSettingsGet", {
      portalId: "builtin",
    });
    assertEquals(before.portalId, "builtin");
    assert(before.version > 0n, "builtin settings carry a bigint version");

    const updated = await runtime.callAdminRpc(
      "authPortalsLoginSettingsUpdate",
      {
        portalId: "builtin",
        expectedVersion: before.version,
        idempotencyKey: ulid(),
        settings: { ...before.settings, localLogin: true },
      },
    );
    assertEquals(updated.version, before.version + 1n);

    const after = await runtime.callAdminRpc("authPortalsLoginSettingsGet", {
      portalId: "builtin",
    });
    assertEquals(after.version, updated.version);
    assertEquals(after.settings.localLogin, true);
  });
});

Deno.test("R06 route edit retains the route ID and leaves exactly one route", async () => {
  await withTrellisRuntime(async (runtime) => {
    const portalId = `ct-r06-${ulid().toLowerCase().slice(-10)}`;
    await runtime.callAdminRpc("authPortalsPut", {
      portalId,
      displayName: "R06 Portal",
      entryUrl: "https://r06.example.com",
      disabled: false,
      expectedVersion: null,
      idempotencyKey: ulid(),
      loginSettings: {
        localLogin: true,
        localRegistration: false,
        federatedRegistration: false,
        providers: null,
      },
    });
    const created = await runtime.callAdminRpc("authPortalsRoutesPut", {
      portalId,
      participantId: null,
      deploymentId: null,
      origin: "https://r06.example.com",
      priority: 10n,
      routeId: null,
      expectedVersion: null,
      idempotencyKey: ulid(),
    });
    assertEquals(created.route.priority, 10n);

    const edited = await runtime.callAdminRpc("authPortalsRoutesPut", {
      portalId,
      participantId: null,
      deploymentId: null,
      origin: "https://r06-edited.example.com",
      priority: 20n,
      routeId: created.route.routeId,
      expectedVersion: created.route.version,
      idempotencyKey: ulid(),
    });
    assertEquals(edited.route.routeId, created.route.routeId);
    assertEquals(edited.route.version, created.route.version + 1n);
    assertEquals(edited.route.priority, 20n);

    const portal = await runtime.callAdminRpc("authPortalsGet", { portalId });
    assertEquals(portal.routes.length, 1);
    assertEquals(portal.routes[0].routeId, created.route.routeId);
    assertEquals(portal.routes[0].origin, "https://r06-edited.example.com");
  });
});

Deno.test("R08 deployment expiry round-trips as a nullable number", async () => {
  await withTrellisRuntime(async (runtime) => {
    const withExpiry = await runtime.callAdminRpc("authDeploymentsCreate", {
      displayName: `ct-r08-${ulid().toLowerCase().slice(-10)}`,
      expiresAt: 4102444800n,
      idempotencyKey: ulid(),
      kind: "service" as const,
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    });
    const fetched = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: withExpiry.deployment.deploymentId,
    });
    assertEquals(fetched.deployment.expiresAt, 4102444800n);

    // A null create intent is accepted and round-trips as null.
    const withoutExpiry = await runtime.callAdminRpc("authDeploymentsCreate", {
      displayName: `ct-r08-null-${ulid().toLowerCase().slice(-10)}`,
      expiresAt: null,
      idempotencyKey: ulid(),
      kind: "service" as const,
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    });
    const fetchedNull = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: withoutExpiry.deployment.deploymentId,
    });
    assertEquals(fetchedNull.deployment.expiresAt, null);

    // Missing/null/malformed required-nullable parsing is proven at the exact
    // production helper boundary by the focused Rust test
    // `nullable_version_helpers_distinguish_missing_null_and_malformed`
    // beside the Auth RPC helpers; this live check proves the real round-trip.
  });
});

Deno.test("R13 sessions list crosses the page boundary without duplication", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const all = await runtime.callAdminRpc("authSessionsList", {
      page: { limit: 100 },
    });
    const ids = all.items.map((item) => item.sessionId);
    assertEquals(new Set(ids).size, ids.length, "no duplicates in one page");
    assert(
      all.items.length <= 100,
      "the requested numeric page size bounds the page",
    );
    for (const item of all.items) {
      assertEquals(typeof item.sessionId, "string");
      assert(item.sessionId.length > 0);
    }
    // A one-row page followed by its cursor must not repeat the first row.
    const first = await runtime.callAdminRpc("authSessionsList", {
      page: { limit: 1 },
    });
    assert(first.items.length === 1, "one real session");
    const cursor = first.nextCursor;
    if (typeof cursor === "string" && cursor.length > 0) {
      const second = await runtime.callAdminRpc("authSessionsList", {
        page: { limit: 1, cursor },
      });
      assert(
        second.items.every((item) =>
          first.items.every((prior) => prior.sessionId !== item.sessionId)
        ),
        "the cursor page must not repeat the previous page",
      );
    }
  });
});

Deno.test("R15 deployment reads track no binding, a binding, and a revoked binding", async () => {
  await withTrellisRuntime(async (runtime) => {
    // A deployment with no participant has no binding, and the read succeeds
    // for the administrator without inventing one.
    const unbound = await runtime.callAdminRpc("authDeploymentsCreate", {
      displayName: `ct-r15-unbound-${ulid().toLowerCase().slice(-10)}`,
      expiresAt: null,
      idempotencyKey: ulid(),
      kind: "service" as const,
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    });
    const unboundRead = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: unbound.deployment.deploymentId,
    });
    assertEquals(unboundRead.binding, null);
    assertEquals(unboundRead.resources.length, 0);

    // A real connected participant produces the binding and its resource
    // evidence, exactly as R02's proven production connection path does.
    const displayName = `ct-r15-${ulid().toLowerCase().slice(-10)}`;
    await runtime.deployments.create({ id: displayName, kind: "service" });
    const key = await runtime.registerService({
      name: displayName,
      contract: participants.Provider.participant,
      deployment: displayName,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      name: displayName,
      seed: key.seed,
    }).orThrow();
    try {
      void service.wait().catch(() => {});
      const deadline = Date.now() + 30_000;
      let boundRead = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: key.deploymentId,
      });
      while (boundRead.resources.length === 0 && Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, 250));
        boundRead = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: key.deploymentId,
        });
      }
      assertExists(boundRead.binding, "the applied deployment has a binding");
      assertEquals(
        boundRead.binding.participantId,
        participants.Provider.participant.id,
      );
      assert(
        boundRead.resources.length > 0,
        "the bound deployment reports resource evidence",
      );

      // Revoking the binding is reflected in later reads: the record is
      // retained with its revoked state and an advanced revision, never a
      // cached active copy, and the deployment read itself still works.
      await runtime.callAdminRpc("authGrantsRevoke", {
        ownerKind: boundRead.binding.ownerKind,
        ownerId: boundRead.binding.ownerId,
        participantId: boundRead.binding.participantId,
        expectedRevision: boundRead.binding.revision,
        idempotencyKey: ulid(),
        reason: "R15 revoked-binding acceptance",
      });
      const revokedRead = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: key.deploymentId,
      });
      assertExists(
        revokedRead.binding,
        "the revoked binding remains readable as revoked",
      );
      assertEquals(revokedRead.binding.state, "revoked");
      assert(
        revokedRead.binding.revision > boundRead.binding.revision,
        "revocation advances the binding revision",
      );
      assertEquals(revokedRead.deployment.deploymentId, key.deploymentId);
    } finally {
      await service.stop();
    }
  });
});
