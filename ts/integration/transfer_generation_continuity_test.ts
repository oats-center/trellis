/**
 * Live transfer continuity across an automatic transport generation.
 *
 * A real public upload transfer is held open while the service's authority
 * grows. Growth must adopt a new generation under the same logical connection,
 * and the transfer accepted on the original generation must finish exactly once
 * with the exact bytes, then clean up.
 */

import { assert, assertEquals } from "@std/assert";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

type Runtime = Parameters<Parameters<typeof withTrellisRuntime>[0]>[0];

/** One admitted attachment as reported by the production admin surface. */
type Attachment = {
  participantId: string;
  runtimeConnectionId: string;
  connectionId: string;
};

async function attachmentsFor(
  runtime: Runtime,
  participantId: string,
): Promise<Attachment[]> {
  const page = await runtime.callAdminRpc("authConnectionsList", {}) as {
    items: Attachment[];
  };
  return page.items.filter((item) => item.participantId === participantId);
}

Deno.test(
  "an in-flight upload transfer finishes with exact bytes across automatic growth",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const providerContract = participants.Provider.participant;
      const providerId = providerContract.identity;

      // 1. Deploy the provider with its optional resource declined.
      await runtime.contracts.install({ contract: providerContract });
      const requested = await runtime.contracts.requestApply({
        contract: providerContract,
      });
      if (requested.status === "approval_required") {
        await runtime.contracts.approveApply(requested.pendingId, {
          excludeResources: ["extras"],
        });
      }
      const instance = await runtime.services.createInstance({
        name: "transfer-generation-provider",
        contract: providerContract,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: providerContract,
        name: "transfer-generation-provider",
        seed: instance.seed,
      }).orThrow();
      const serviceExit = service.wait().catch((error: unknown) => error);

      // 2. The upload handler holds the accepted transfer before reading it.
      const releaseUpload = Promise.withResolvers<void>();
      let uploadStarted = 0;
      await service.handleUpload(async ({ input, op, transfer }) => {
        uploadStarted += 1;
        await releaseUpload.promise;
        const body = await transfer.stream().orThrow();
        const store = await service.store.files.open().orThrow();
        await store.put(input.value, body).orThrow();
        return await op.complete(input).orThrow();
      });

      const caller = await runtime.connectClient({
        name: "transfer-generation-caller",
        contract: participants.Caller.participant,
        timeout: 120_000,
      });

      try {
        const [before] = await attachmentsFor(runtime, providerId);
        assert(before, "the provider must have a physical attachment");
        const logical = before.runtimeConnectionId;
        const physical = before.connectionId;

        // 3. Start a real upload transfer and hold it while the service grows.
        const bytes = Uint8Array.from(
          { length: 131_073 },
          (_, index) => index % 251,
        );
        const uploading = caller.upload({ value: "held-bytes" })
          .transfer(bytes)
          .start()
          .orThrow();
        await runtime.waitFor(() => uploadStarted === 1, { timeoutMs: 30_000 });

        // 4. Grow authority: the service adopts a wider generation.
        await runtime.contracts.apply({ contract: providerContract });
        const adopted = await runtime.waitFor(async () => {
          const items = (await attachmentsFor(runtime, providerId)).filter(
            (item) => item.runtimeConnectionId === logical,
          );
          return items.length >= 2 ? items : false;
        }, { timeoutMs: 90_000 });
        assert(
          adopted.some((item) => item.connectionId !== physical),
          "authority growth must adopt a new physical generation",
        );

        // 5. Finish the held transfer: exact bytes, delivered exactly once.
        releaseUpload.resolve();
        const operation = await uploading;
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.terminal.state, "completed");
        assertEquals(terminal.transferred.size, bytes.length);
        const entry = await service.store.files.waitFor("held-bytes")
          .orThrow();
        assertEquals(await entry.bytes().orThrow(), bytes);
        assertEquals(uploadStarted, 1);
        assertEquals(service.connection.status.phase, "connected");
      } finally {
        releaseUpload.resolve();
        await caller.connection.close().catch(() => undefined);
        await service.stop();
        await serviceExit;
      }
    }, {
      authorization: {
        contextLifetimeSeconds: 76,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 46,
      },
    });
  },
);
