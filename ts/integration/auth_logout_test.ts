import { AuthorizationContextRefreshError } from "@oatscenter/trellis/auth";
import { assertEquals, assertExists, assertRejects } from "@std/assert";
import { participants } from "trellis-web-generated";
import { ulid } from "ulid";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("logout confirms a revoked session after its transport has closed", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "already-revoked-logout",
      contract: participants.Console.participant,
    });
    const session = (await client.sessionsMe({}).orThrow()).session;
    assertExists(session);
    await runtime.callAdminRpc("authSessionsRevoke", {
      sessionId: session.sessionId,
      expectedVersion: session.version,
      idempotencyKey: ulid(),
      reason: "Verify logout after physical closure",
    });
    await client.connection.close();
    await client.logout();
    assertEquals(
      (await runtime.callAdminRpc("authSessionsList", {
        participantId: session.participantId,
      })).items.find((entry) => entry.sessionId === session.sessionId)?.state,
      "revoked",
    );
  });
});

Deno.test("logout does not mistake revoked authority for session revocation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "authority-revoked-logout",
      contract: participants.Console.participant,
    });
    const session = (await client.sessionsMe({}).orThrow()).session;
    assertExists(session);
    const { binding } = await runtime.callAdminRpc("authGrantsGet", {
      ownerKind: "user",
      ownerId: session.principalId,
      participantId: session.participantId,
    });
    assertExists(binding);
    await runtime.callAdminRpc("authGrantsRevoke", {
      ownerKind: "user",
      ownerId: session.principalId,
      participantId: session.participantId,
      expectedRevision: binding.revision,
      idempotencyKey: ulid(),
    });
    await client.connection.close();
    await assertRejects(
      () => client.logout(),
      AuthorizationContextRefreshError,
      "not_authorized",
    );
    assertEquals(
      (await runtime.callAdminRpc("authSessionsList", {
        participantId: session.participantId,
      })).items.find((entry) => entry.sessionId === session.sessionId)?.state,
      "active",
    );
  });
});
