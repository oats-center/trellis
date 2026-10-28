import assert from "node:assert/strict";
import { test } from "node:test";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

for (const expireAdmission of [false, true]) {
  test(`native required and optional RPCs recover after ${expireAdmission ? "expired" : "unexpired"} admission and host restart`, async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const client = await runtime.connectClient({
          name: "restart-reader",
          contract: adminParticipant,
        });
        try {
          const before = await client.sessionsMe({}).orThrow();
          assert(before.login);
          await client.eventsQuery({}).orThrow();
          if (expireAdmission) {
            const deadline = client.ownAuthority.current?.expiresAt;
            assert(deadline !== undefined);
            // Exercise expiry on the real clock using ordinary short-lease config,
            // while normal NATS authority renewal keeps the current client usable.
            await runtime.waitFor(() => Date.now() > (deadline + 1) * 1000, {
              timeoutMs: Math.max(0, deadline * 1000 - Date.now()) + 5_000,
            });
            await client.sessionsMe({}).orThrow();
          }
          await runtime.restart();
          const after = await runtime.waitFor(
            async () => {
              const required = await client.sessionsMe({}, { timeout: 1000 });
              const optional = await client.eventsQuery({}, { timeout: 1000 });
              if (required.isErr() || optional.isErr()) {
                throw new Error(
                  JSON.stringify({
                    required: required.isErr()
                      ? required.error.toSerializable()
                      : "usable",
                    optional: optional.isErr()
                      ? optional.error.toSerializable()
                      : "usable",
                    authority: client.ownAuthority.current,
                  }),
                );
              }
              return required.orThrow();
            },
            { timeoutMs: 30_000 },
          );
          assert(after.login);
          assert.equal(after.login.loginSessionId, before.login.loginSessionId);
          assert.equal(
            after.authorizationSessionId,
            before.authorizationSessionId,
          );
        } finally {
          await client.connection.close();
        }
      },
      expireAdmission
        ? {
            authorization: {
              contextLifetimeSeconds: 27,
              allowedClockSkewSeconds: 2,
            },
          }
        : {},
    );
  });
}
