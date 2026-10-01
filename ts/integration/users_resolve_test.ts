import { assert, assertEquals } from "@std/assert";
import { participants } from "trellis-web-generated";
import { ulid } from "ulid";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("generated Users.Resolve retrieves a named account through its bytes selector", async () => {
  await withTrellisRuntime(async (runtime) => {
    const client = await runtime.connectClient({
      name: "user-resolution-console",
      contract: participants.Console.participant,
    });
    const created = await client.usersCreate({
      email: "reviewer@example.com",
      idempotencyKey: ulid(),
      image: null,
      name: "Type access reviewer",
      username: "type-access-reviewer",
    }).orThrow();
    const resolved = await client.usersResolve({
      selector: new TextEncoder().encode(JSON.stringify({
        kind: "user",
        userId: created.user.userId,
      })),
    }).orThrow();
    assertEquals(resolved.user, created.user);

    const byProvider = await client.usersResolve({
      selector: new TextEncoder().encode(JSON.stringify({
        kind: "provider",
        providerId: "local",
        providerSubject: "type-access-reviewer",
      })),
    }).orThrow();
    assertEquals(byProvider.user, created.user);

    assert((await client.usersResolve({
      selector: new TextEncoder().encode("not JSON"),
    })).isErr());
    assert((await client.usersResolve({
      selector: new TextEncoder().encode(
        JSON.stringify({ kind: "unsupported" }),
      ),
    })).isErr());
  });
});
