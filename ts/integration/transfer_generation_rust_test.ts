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
      { length: 8 * 1024 * 1024 },
      (_, index) => index % 251,
    );
    let held: ReadableStreamDefaultReader<Uint8Array> | undefined;
    let idle: ReadableStreamDefaultReader<Uint8Array> | undefined;
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
      // Rust reads the runtime-owned staged object and validates every byte
      // before completing: a successful terminal proves the real TS -> Rust path.
      const uploadProgress: number[] = [];
      const uploaded = await caller.upload({ value: "cross-language" })
        .transfer(bytes)
        .onTransfer(({ transfer }) => {
          uploadProgress.push(transfer.transferredBytes);
        })
        .start().orThrow();
      const uploadTerminal = await uploaded.wait().orThrow();
      assertEquals(uploadTerminal.terminal.state, "completed");
      assertEquals(uploadTerminal.terminal.output?.value, "cross-language");
      assertEquals(uploadTerminal.transferred.size, bytes.length);
      assertEquals(
        uploadTerminal.terminal.transfer?.transferredBytes,
        bytes.length,
      );
      assert(uploadProgress.some((value) => value > 0));
      for (let index = 1; index < uploadProgress.length; index += 1) {
        assert(uploadProgress[index] >= uploadProgress[index - 1]);
      }
      const before = await caller.download({ value: "before" }).orThrow();
      const beforeGrant = Value.Parse(TransferGrantSchema, before.transfer);
      assert(beforeGrant.direction === "receive");
      const unauthorized = await runtime.connectClient({
        name: "unrelated-transfer-caller",
        contract: participants.Caller.participant,
      });
      try {
        assert(
          (await unauthorized.transfer(beforeGrant).bytes()).isErr(),
          "a grant cannot be consumed by another authenticated session",
        );
      } finally {
        await unauthorized.connection.close();
      }
      assertEquals(
        await caller.transfer(beforeGrant).bytes().orThrow(),
        bytes,
      );

      // Stop public stream consumption after actual DATA, before growth.
      const ongoing = await caller.download({ value: "ongoing" }).orThrow();
      const ongoingGrant = Value.Parse(TransferGrantSchema, ongoing.transfer);
      assert(ongoingGrant.direction === "receive");
      held = (await caller.transfer(ongoingGrant).stream().orThrow())
        .getReader();
      const first = await held.read();
      assert(!first.done && first.value.length > 0);
      const cancelled = await caller.download({ value: "cancelled" }).orThrow();
      const cancelledGrant = Value.Parse(
        TransferGrantSchema,
        cancelled.transfer,
      );
      assert(cancelledGrant.direction === "receive");
      const chunks: Uint8Array[] = [first.value];
      await runtime.contracts.apply({ contract });
      await runtime.waitFor(
        async () =>
          (await attachments()).some((item) =>
            item.runtimeConnectionId === initial.runtimeConnectionId &&
            item.connectionId !== initial.connectionId
          ),
        { timeoutMs: 90_000 },
      );
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
      const digest = `SHA-256=${
        btoa(
          String.fromCharCode(
            ...new Uint8Array(await crypto.subtle.digest("SHA-256", received)),
          ),
        ).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "")
      }`;
      assertEquals(ongoingGrant.info.digest, digest);
      assertEquals(uploadTerminal.transferred.digest, digest);

      // The unused grant still owns its accepting attachment until cancellation.
      assert(
        (await attachments()).some((item) =>
          item.connectionId === initial.connectionId
        ),
      );
      const cancelledStream = await caller.transfer(cancelledGrant).stream()
        .orThrow();
      const cancelReader = cancelledStream.getReader();
      assert(!(await cancelReader.read()).done);
      await cancelReader.cancel();
      assert((await cancelReader.read()).done);
      cancelReader.releaseLock();

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

      // Physical transport closure must fail a consumer that never pulls again.
      // Observe reader.closed, not another read or a public cancellation that
      // would itself drive cleanup and conceal the idle-abort regression.
      const idleDownload = await caller.download({ value: "idle-disconnect" })
        .orThrow();
      const idleGrant = Value.Parse(TransferGrantSchema, idleDownload.transfer);
      assert(idleGrant.direction === "receive");
      idle = (await caller.transfer(idleGrant).stream().orThrow()).getReader();
      const idleFirst = await idle.read();
      assert(!idleFirst.done && idleFirst.value.length > 0);
      let idleClosed: "pending" | "completed" | "failed" = "pending";
      void idle.closed.then(
        () => {
          idleClosed = "completed";
        },
        () => {
          idleClosed = "failed";
        },
      );
      await caller.connection.close();
      await runtime.waitFor(() => idleClosed !== "pending");
      assertEquals(
        idleClosed,
        "failed",
        "physical loss must not imply verified EOF",
      );
    } finally {
      await idle?.cancel().catch(() => undefined);
      idle?.releaseLock();
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
