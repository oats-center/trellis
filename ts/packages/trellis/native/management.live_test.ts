import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { test } from "node:test";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { ulid } from "ulid";

import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { apis, apps } from "../../trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../native.ts";

test(
  "role entitlements and optional consent remain distinct; signed nonadmin mutations are denied",
  { timeout: 45_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;

      const directory = await mkdtemp("/tmp/opencode/native-management-");
      const connections: Array<() => Promise<unknown>> = [];
      try {
        const request = apps.cli.request;
        const adminLogin = await loginNativeTestClient({
          issuer,
          username,
          password,
          directory,
          clientId: "trellis.cli",
          request,
          platformPrivileges: {
            required: ["principals.manage", "roles.manage", "clients.manage"],
          },
          signal: context.signal,
        });
        const admin = await connectNativeClient({
          ...adminLogin,
          request,
          signal: context.signal,
        }).orThrow();
        connections.push(() => admin.connection.close());
        const suffix = ulid();
        const userPassword = `management-${suffix}`;
        const { user } = await admin
          .usersCreate({
            username: `management-${suffix}`,
            password: userPassword,
            displayName: "Limited management caller",
            idempotencyKey: ulid(),
          })
          .orThrow();
        const approved = [
          ...new Set([
            ...request.requiredCapabilities,
            "trellis.auth@v1::sessionsRead",
            "trellis.auth@v1::rolesPut",
          ]),
        ];
        const declined = "trellis.auth@v1::capabilityGrantsRead";
        const { role } = await admin
          .rolesPut({
            title: `Management ${suffix}`,
            description:
              "Selected application capabilities confer no administrative privilege",
            capabilityIds: [...approved, declined],
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        await admin
          .roleAssignmentsAssign({
            principalId: user.principal.principalId,
            roleId: role.roleId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const clientId = `management-${suffix}`;
        await admin
          .oAuthClientsPut({
            clientId,
            title: "Limited management caller",
            description: "Explicit optional subset",
            kind: "native",
            redirectUris: [],
            development: true,
            impliedConsent: [],
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const login = await loginNativeTestClient({
          issuer,
          username: user.username,
          password: userPassword,
          directory,
          clientId,
          request,
          approvedCapabilities: approved,
          signal: context.signal,
        });
        const caller = await connectNativeClient({
          ...login,
          request,
          signal: context.signal,
        }).orThrow();
        connections.push(() => caller.connection.close());
        const sessions = await caller.sessionsList({}).orThrow();
        assert.ok(
          sessions.items.some(
            (session) => session.principalId === user.principal.principalId,
          ),
        );
        // rolesPut was granted and explicitly approved: this is a signed request
        // for an available action, rejected by the authoritative privilege check.
        const attempted = await caller.rolesPut({
          roleId: role.roleId,
          title: "Unauthorized overwrite",
          description: "",
          capabilityIds: [],
          expectedRevision: role.revision,
          idempotencyKey: ulid(),
        });
        assert.equal(attempted.isErr(), true);
        if (attempted.isErr()) {
          assert.ok(attempted.error instanceof apis.auth.AuthError);
          assert.equal(attempted.error.data.code, "access_denied");
        }
        const unchanged = await admin
          .rolesGet({ roleId: role.roleId })
          .orThrow();
        assert.equal(unchanged.role.title, role.title);
        const unapproved = await caller.capabilityGrantsList({
          principalId: user.principal.principalId,
        });
        assert.equal(
          unapproved.isErr(),
          true,
          "an unapproved optional capability must not become usable through role membership",
        );
      } finally {
        for (const close of connections.reverse()) await close();
        await rm(directory, { recursive: true, force: true });
      }
    });
  },
);
