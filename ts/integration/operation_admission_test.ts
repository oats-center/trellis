import { TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals } from "@std/assert";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("concurrent admission preserves cancellation, frozen input and creator", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "atomic-admission",
      contract: participants.Provider.participant,
    });
    const replica = await runtime.services.createInstance({
      name: "atomic-admission-replica",
      contract: participants.Provider.participant,
    });
    const services = await Promise.all(
      [identity, replica].map((instance) =>
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "atomic-admission",
          seed: instance.seed,
        }).orThrow()
      ),
    );
    const client = await runtime.connectClient({
      name: "atomic-admission-caller",
      contract: participants.Caller.participant,
    });
    const foreignIdentity = await runtime.registerService({
      name: "atomic-admission-foreign-caller",
      deployment: "foreign-caller",
      contract: participants.OperationCaller.participant,
    });
    const foreign = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.OperationCaller.participant,
      name: "atomic-admission-foreign-caller",
      seed: foreignIdentity.seed,
    }).orThrow();
    let businessExecutions = 0;
    let expectedInput = "";
    let creator: string | undefined;
    let cleanupEntered = Promise.withResolvers<void>();
    let releaseCleanup = Promise.withResolvers<void>();
    for (const service of services) {
      await service.handleWork(async ({ input, caller, signal }) => {
        assertEquals(input.value, expectedInput);
        assert(caller.type === "verified");
        const identity = `${caller.principalId}:${caller.participantId}`;
        creator ??= identity;
        assertEquals(identity, creator);
        if (!signal.aborted) {
          businessExecutions++;
          await new Promise<void>((resolve) => {
            signal.addEventListener("abort", () => resolve(), { once: true });
          });
        }
        cleanupEntered.resolve();
        await releaseCleanup.promise;
      });
    }
    await services[0].handleUpload(async ({ input, signal, transfer, op }) => {
      if (signal.aborted) return;
      if (!transfer) throw new Error("ordinary upload requires a transfer");
      const body = await transfer.stream().orThrow();
      const store = await services[0].store.files.open().orThrow();
      await store.put(input.value, body).orThrow();
      return await op.complete(input).orThrow();
    });
    try {
      for (let round = 0; round < 4; round++) {
        const invocationId = ulid();
        expectedInput = `frozen-${invocationId}`;
        businessExecutions = 0;
        cleanupEntered = Promise.withResolvers<void>();
        releaseCleanup = Promise.withResolvers<void>();
        const admissions = await Promise.all(
          Array.from(
            { length: 12 },
            (_, index) =>
              client.work({ value: expectedInput }).start(undefined, {
                invocationId,
                cancellationRequested: (index + round) % 3 === 0,
              }).orThrow(),
          ),
        );
        const operation = admissions[0];
        for (const admission of admissions) {
          assertEquals(admission.id, invocationId);
        }
        await cleanupEntered.promise;
        assert(businessExecutions <= 1);
        assertEquals((await operation.get().orThrow()).state, "pending");
        const conflicting = await Promise.all([
          client.work({ value: "different-input" }).start(undefined, {
            invocationId,
            cancellationRequested: true,
          }),
          foreign.work({ value: expectedInput }).start(undefined, {
            invocationId,
            cancellationRequested: true,
          }),
        ]);
        for (const conflict of conflicting) assert(conflict.isErr());
        await client.work({ value: expectedInput }).start(undefined, {
          invocationId,
        }).orThrow();
        releaseCleanup.resolve();
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.state, "cancelled");
        for (const admission of admissions) {
          assertEquals(await admission.get().orThrow(), terminal);
        }
        assert(businessExecutions <= 1);
      }
      const contestedId = ulid();
      expectedInput = `contested-${contestedId}`;
      businessExecutions = 0;
      creator = undefined;
      cleanupEntered = Promise.withResolvers<void>();
      releaseCleanup = Promise.withResolvers<void>();
      const contested = await Promise.all([
        client.work({ value: expectedInput }).start(undefined, {
          invocationId: contestedId,
          cancellationRequested: true,
        }),
        foreign.work({ value: expectedInput }).start(undefined, {
          invocationId: contestedId,
          cancellationRequested: true,
        }),
      ]);
      const winners = contested.filter((outcome) => outcome.isOk());
      assertEquals(winners.length, 1);
      const winner = winners[0];
      assert(winner.isOk());
      await cleanupEntered.promise;
      assertEquals(businessExecutions, 0);
      releaseCleanup.resolve();
      assertEquals(
        (await winner.orThrow().wait().orThrow()).state,
        "cancelled",
      );
      const bytes = Uint8Array.from(
        { length: 131073 },
        (_, index) => index % 251,
      );
      const upload = await client.upload({ value: "admission-upload" })
        .transfer(bytes).start().orThrow();
      const uploaded = await upload.wait().orThrow();
      assertEquals(uploaded.terminal.state, "completed");
      assertEquals(uploaded.terminal.output, { value: "admission-upload" });
      assertEquals(uploaded.transferred.size, bytes.length);
      const stored = await services[0].store.files.waitFor("admission-upload")
        .orThrow();
      assertEquals(await stored.bytes().orThrow(), bytes);
    } finally {
      releaseCleanup.resolve();
      await client.connection.close();
      await foreign.stop();
      await foreign.wait();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(services.map((service) => service.wait()));
    }
  });
});
