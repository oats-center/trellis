import { assert, assertEquals } from "@std/assert";
import { TransferGrantSchema } from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("Rust downloads survive growth and open after the original attachment closes", async () => {
  await withTrellisRuntime(async (runtime) => {
    const contract = participants.Provider.participant;
    await runtime.contracts.install({ contract });
    const requested = await runtime.contracts.requestApply({ contract });
    assertEquals(requested.status, "approval_required");
    if (requested.status !== "approval_required") {
      throw new Error("missing approval");
    }
    await runtime.contracts.approveApply(requested.pendingId, {
      excludeResources: ["extras"],
    });
    const instance = await runtime.services.createInstance({
      name: "rust-transfer-generation",
      contract,
    });
    const caller = await runtime.connectClient({
      name: "rust-transfer-caller",
      contract: participants.Caller.participant,
    });
    const argv = rustFixtureArgv("transfer_generation");
    const child = new Deno.Command(argv[0], {
      args: argv.slice(1),
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: instance.seed,
      },
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stdout = child.stdout.pipeThrough(new TextDecoderStream())
      .getReader();
    let output = "";
    let outputEnded = false;
    const outputTask = (async () => {
      for (;;) {
        const chunk = await stdout.read();
        if (chunk.done) break;
        output += chunk.value;
      }
    })().finally(() => {
      outputEnded = true;
      stdout.releaseLock();
    });
    // Observe read failures when joining cleanup, without an unhandled rejection.
    outputTask.catch(() => undefined);
    const bytes = Uint8Array.from(
      { length: 131_073 },
      (_, index) => index % 251,
    );
    let held: ReadableStreamDefaultReader<Uint8Array> | undefined;
    try {
      await runtime.waitFor(() => {
        if (outputEnded) {
          throw new Error(`provider ended before ready: ${output}`);
        }
        return output.includes("transfer provider ready");
      });
      const attachments = async () => {
        const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
          items: {
            participantId: string;
            runtimeConnectionId: string;
            connectionId: string;
          }[];
        };
        return page.items.filter((item) =>
          item.participantId === contract.identity
        );
      };
      const [initial] = await attachments();
      assert(initial);
      const before = await caller.download({ value: "before" }).orThrow();
      const beforeGrant = Value.Parse(TransferGrantSchema, before.transfer);
      assert(beforeGrant.direction === "receive");
      assertEquals(
        await caller.transfer(beforeGrant).bytes().orThrow(),
        bytes,
      );

      // Hold an admitted grant before its first pull: a store reader must not
      // accidentally keep the endpoint's original attachment alive for it.
      const ongoing = await caller.download({ value: "ongoing" }).orThrow();
      const ongoingGrant = Value.Parse(TransferGrantSchema, ongoing.transfer);
      assert(ongoingGrant.direction === "receive");
      const cancelled = await caller.download({ value: "cancelled" }).orThrow();
      const cancelledGrant = Value.Parse(
        TransferGrantSchema,
        cancelled.transfer,
      );
      assert(cancelledGrant.direction === "receive");
      const chunks: Uint8Array[] = [];
      await runtime.contracts.apply({ contract });
      await runtime.waitFor(
        async () =>
          (await attachments()).some((item) =>
            item.runtimeConnectionId === initial.runtimeConnectionId &&
            item.connectionId !== initial.connectionId
          ),
        { timeoutMs: 90_000 },
      );
      held = (await caller.transfer(ongoingGrant).stream().orThrow())
        .getReader();
      for (;;) {
        const chunk = await held.read();
        if (chunk.done) break;
        chunks.push(chunk.value);
      }
      const received = new Uint8Array(
        chunks.reduce((size, chunk) => size + chunk.length, 0),
      );
      let offset = 0;
      for (const chunk of chunks) {
        received.set(chunk, offset);
        offset += chunk.length;
      }
      assertEquals(received, bytes);

      // The unused grant still owns its accepting attachment until cancellation.
      assert(
        (await attachments()).some((item) =>
          item.connectionId === initial.connectionId
        ),
      );
      const cancelledStream = await caller.transfer(cancelledGrant).stream()
        .orThrow();
      await cancelledStream.cancel();

      // Observe actual retirement, not a sleep or an application-requested refresh.
      await runtime.waitFor(
        async () =>
          !(await attachments()).some((item) =>
            item.connectionId === initial.connectionId
          ),
        { timeoutMs: 120_000 },
      );
      const after = await caller.download({ value: "after" }).orThrow();
      const afterGrant = Value.Parse(TransferGrantSchema, after.transfer);
      assert(afterGrant.direction === "receive");
      assertEquals(
        await caller.transfer(afterGrant).bytes().orThrow(),
        bytes,
      );
    } finally {
      await held?.cancel().catch(() => undefined);
      held?.releaseLock();
      await caller.connection.close();
      try {
        child.kill("SIGTERM");
      } catch { /* process already exited */ }
      await child.status;
      await outputTask;
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
  });
});
