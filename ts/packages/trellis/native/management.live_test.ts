import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { ulid } from "ulid";

import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { apis, apps } from "../../trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../native.ts";

test(
  "role entitlements and optional consent remain distinct; delegated administration reaches the app once approved",
  { timeout: 45_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;

      const connections: Array<() => Promise<unknown>> = [];
      try {
        const request = apps.cli.request;
        const adminLogin = await loginNativeTestClient({
          issuer,
          username,
          password,
          clientId: "trellis.cli",
          request,
          administrativeCapabilities: [
            "trellis.auth.v1:users.manage",
            "trellis.auth.v1:roles.manage",
            "trellis.auth.v1:clients.manage",
            "trellis.auth.v1:platform.manage",
          ],
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
        assert(user.account);
        const approved = [
          ...new Set([
            ...request.requiredCapabilities,
            "trellis.auth.v1:sessions.read",
          ]),
        ];
        const declined = "trellis.auth.v1:roles.manage";
        const { role } = await admin
          .rolesPut({
            title: `Management ${suffix}`,
            description:
              "Role membership is not consent to delegate administration",
            capabilityIds: [...approved, declined],
            assignToNewUsers: false,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        await admin
          .roleAssignmentsAssign({
            principalId: user.principalId,
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
          username: user.account.username,
          password: userPassword,
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
            (session) => session.principalId === user.principalId,
          ),
        );
        // The role carries manageRoles, but this login declined it.
        const declinedAttempt = await caller.rolesPut({
          roleId: role.roleId,
          title: "Unauthorized overwrite",
          description: "",
          capabilityIds: [],
          assignToNewUsers: false,
          expectedRevision: role.revision,
          idempotencyKey: ulid(),
        });
        assert.equal(declinedAttempt.isErr(), true);
        assert.equal(
          (
            await caller.capabilityGrantsList({ principalId: user.principalId })
          ).isErr(),
          true,
          "an unapproved optional capability must not become usable through role membership",
        );
        // Approving it delegates it at once.
        const delegating = await loginNativeTestClient({
          issuer,
          username: user.account.username,
          password: userPassword,
          clientId,
          request,
          approvedCapabilities: [...approved, declined],
          signal: context.signal,
        });
        const delegated = await connectNativeClient({
          ...delegating,
          request,
          signal: context.signal,
        }).orThrow();
        connections.push(() => delegated.connection.close());
        // Roles without administration need only the delegated roles.manage.
        const { role: created } = await delegated
          .rolesPut({
            title: `Delegated ${suffix}`,
            description: "",
            capabilityIds: [],
            assignToNewUsers: false,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const stored = await admin
          .rolesGet({ roleId: created.roleId })
          .orThrow();
        assert.equal(stored.role.title, `Delegated ${suffix}`);
      } finally {
        for (const close of connections.reverse()) await close();
      }
    });
  },
);
