import assert from "node:assert/strict";
import { Readable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { decodeTransferGrant, Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { ulid } from "ulid";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("Rust downloads survive growth and open after the original attachment closes", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.TransferGrowthProvider.participant;
      const instance = await runtime.registerService({
        deployment: "rust-transfer-generation",
        name: "rust-transfer-generation",
        contract,
      });
      const capabilityId = "runtime-trellis.transport_growth@v1::extend";
      const grants = await runtime.callAdminRpc("authCapabilityGrantsList", {
        capabilityId,
      });
      assert.equal(grants.items.length, 1);
      const grant = grants.items[0];
      await runtime.callAdminRpc("authCapabilityGrantsRevoke", {
        grantId: grant.grantId,
        expectedRevision: grant.revision,
        idempotencyKey: ulid(),
      });
      const caller = await runtime.connectClient({
        name: "rust-transfer-caller",
        contract: apps.Caller.request,
      });
      const argv = rustFixtureArgv("transfer_generation");
      const child = spawnTracked(argv[0], argv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: instance.seed,
        },
      });
      child.stderr.pipe(process.stderr);
      const stdout = Readable.toWeb(child.stdout)
        .pipeThrough(new TextDecoderStream())
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
      const targetIdentity = await runtime.registerService({
        deployment: "transfer-growth-target",
        name: "transfer-growth-target",
        contract: participants.TransportGrowthTarget.participant,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.TransportGrowthTarget.participant,
        seed: targetIdentity.seed,
      }).orThrow();
      const targetExit = target.wait().catch((error: unknown) => error);
      try {
        await target.handleExtend(() => Result.ok({}));
        await runtime.waitFor(() => {
          if (outputEnded) {
            throw new Error(`provider ended before ready: ${output}`);
          }
          return output.includes("transfer provider ready");
        });
        const attachments = async () => {
          const sessions = await runtime.callAdminRpc(
            "authAuthorizationSessionsList",
            {},
          );
          const owned = new Set(
            sessions.items
              .filter(
                (item) =>
                  item.binding.kind === "service" &&
                  item.binding.deploymentId === instance.deploymentId,
              )
              .map((item) => item.authorizationSessionId),
          );
          const page = await runtime.callAdminRpc("authConnectionsList", {});
          return page.items.filter(
            (item) =>
              item.state === "admitted" &&
              owned.has(item.authorizationSessionId),
          );
        };
        const [initial] = await attachments();
        assert(initial);
        // Rust reads the runtime-owned staged object and validates every byte
        // before completing: a successful terminal proves the real TS -> Rust path.
        const uploadProgress: number[] = [];
        const uploaded = await caller
          .upload({ value: "cross-language" })
          .transfer(bytes)
          .onTransfer(({ transfer }) => {
            uploadProgress.push(transfer.transferredBytes);
          })
          .start()
          .orThrow();
        const uploadTerminal = await uploaded.wait().orThrow();
        assert.deepEqual(uploadTerminal.terminal.state, "completed");
        assert.deepEqual(
          uploadTerminal.terminal.output?.value,
          "cross-language",
        );
        assert.deepEqual(uploadTerminal.transferred.size, bytes.length);
        assert.deepEqual(
          uploadTerminal.terminal.transfer?.transferredBytes,
          bytes.length,
        );
        assert(uploadProgress.some((value) => value > 0));
        for (let index = 1; index < uploadProgress.length; index += 1) {
          assert(uploadProgress[index] >= uploadProgress[index - 1]);
        }
        const before = await caller.download({ value: "before" }).orThrow();
        const beforeGrant = decodeTransferGrant(before.transfer).orThrow();
        assert(beforeGrant.direction === "receive");
        const unauthorized = await runtime.connectClient({
          name: "unrelated-transfer-caller",
          contract: apps.Caller.request,
        });
        try {
          assert(
            (await unauthorized.transfer(beforeGrant).bytes()).isErr(),
            "a grant cannot be consumed by another authenticated session",
          );
        } finally {
          await unauthorized.connection.close();
        }
        assert.deepEqual(
          await caller.transfer(beforeGrant).bytes().orThrow(),
          bytes,
        );

        // Stop public stream consumption after actual DATA, before growth.
        const ongoing = await caller.download({ value: "ongoing" }).orThrow();
        const ongoingGrant = decodeTransferGrant(ongoing.transfer).orThrow();
        assert(ongoingGrant.direction === "receive");
        held = (
          await caller.transfer(ongoingGrant).stream().orThrow()
        ).getReader();
        const first = await held.read();
        assert(!first.done && first.value.length > 0);
        const cancelled = await caller
          .download({ value: "cancelled" })
          .orThrow();
        const cancelledGrant = decodeTransferGrant(
          cancelled.transfer,
        ).orThrow();
        assert(cancelledGrant.direction === "receive");
        const chunks: Uint8Array[] = [first.value];
        await runtime.callAdminRpc("authCapabilityGrantsGrant", {
          principalId: grant.principalId,
          capabilityId,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        });
        child.stdin.write("REFRESH\n");
        await runtime.waitFor(() =>
          output.includes("transfer authority refreshed"),
        );
        await runtime.waitFor(
          async () =>
            (await attachments()).some(
              (item) =>
                item.sessionPublicKey === initial.sessionPublicKey &&
                item.attachmentId !== initial.attachmentId,
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
        assert.deepEqual(received, bytes);
        const digest = `SHA-256=${btoa(
          String.fromCharCode(
            ...new Uint8Array(await crypto.subtle.digest("SHA-256", received)),
          ),
        )
          .replaceAll("+", "-")
          .replaceAll("/", "_")
          .replace(/=+$/, "")}`;
        assert.deepEqual(ongoingGrant.info.digest, digest);
        assert.deepEqual(uploadTerminal.transferred.digest, digest);

        // The unused grant still owns its accepting attachment until cancellation.
        assert(
          (await attachments()).some(
            (item) => item.attachmentId === initial.attachmentId,
          ),
        );
        const cancelledStream = await caller
          .transfer(cancelledGrant)
          .stream()
          .orThrow();
        const cancelReader = cancelledStream.getReader();
        assert(!(await cancelReader.read()).done);
        await cancelReader.cancel();
        assert((await cancelReader.read()).done);
        cancelReader.releaseLock();

        // Observe actual retirement, not a sleep or an application-requested refresh.
        await runtime.waitFor(
          async () =>
            !(await attachments()).some(
              (item) => item.attachmentId === initial.attachmentId,
            ),
          { timeoutMs: 120_000 },
        );
        const after = await caller.download({ value: "after" }).orThrow();
        const afterGrant = decodeTransferGrant(after.transfer).orThrow();
        assert(afterGrant.direction === "receive");
        assert.deepEqual(
          await caller.transfer(afterGrant).bytes().orThrow(),
          bytes,
        );

        // Physical transport closure must fail a consumer that never pulls again.
        // Observe reader.closed, not another read or a public cancellation that
        // would itself drive cleanup and conceal the idle-abort regression.
        const idleDownload = await caller
          .download({ value: "idle-disconnect" })
          .orThrow();
        const idleGrant = decodeTransferGrant(idleDownload.transfer).orThrow();
        assert(idleGrant.direction === "receive");
        idle = (
          await caller.transfer(idleGrant).stream().orThrow()
        ).getReader();
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
        assert.deepEqual(
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
        } catch {
          /* process already exited */
        }
        await child.exited;
        await outputTask;
        await target.stop();
        await targetExit;
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 60,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 0,
        minimumContextLifetimeSeconds: 26,
        allowedClockSkewSeconds: 2,
      },
    },
  );
});
