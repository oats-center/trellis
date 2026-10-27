import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { Result } from "@oatscenter/trellis";
import { inspect } from "node:util";
import { transferConstants } from "../packages/trellis/auth/protocol_wasm.ts";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "a process admits receive windows up to its budget and frees them when a transfer ends",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "admission-provider",
        deployment: "admission-provider",
        contract: participants.OperationProvider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.OperationProvider.participant,
        seed: identity.seed,
      }).orThrow();
      // Each generated download is larger than one window, so an unread
      // receiver keeps its whole admitted window occupied.
      const C = transferConstants();
      const size = C.windowBytes * 2;
      const bytes = Uint8Array.from({ length: size }, (_, i) => i % 251);
      const digest = `SHA-256=${createHash("sha256").update(bytes).digest("base64url")}`;
      await service.handleGenerate(async ({ input, context }) => {
        assert(context.caller.type === "verified");
        const transfer = await service
          .createTransfer({
            direction: "receive",
            stream: (async function* () {
              yield bytes;
            })(),
            info: {
              key: `generated-${input.value}`,
              size,
              updatedAt: new Date().toISOString(),
              digest,
              metadata: {},
            },
            context,
          })
          .orThrow();
        return Result.ok({ ...input, transfer });
      });
      const client = await runtime.connectClient({
        name: "admission-caller",
        contract: apps.Caller.request,
      });
      const open = async (value: string) => {
        const response = await client.generate({ value }).orThrow();
        const grant = response.transfer;
        assert(grant);
        return await client.transfer(grant).stream();
      };
      const readers: ReadableStreamDefaultReader<Uint8Array>[] = [];
      try {
        await runtime.waitFor(async () => {
          const probe = await open("probe");
          if (probe.isErr()) return false;
          for await (const _ of probe.take() as ReadableStream<Uint8Array>) {
            /* drain */
          }
          return true;
        });
        // Fill the process budget with windows held open by unread readers.
        const capacity = Math.floor(C.processReceiveBytes / C.windowBytes);
        for (let index = 0; index < capacity; index++) {
          const stream = (await open(String(index))).orThrow();
          const reader = stream.getReader();
          assert(!(await reader.read()).done);
          readers.push(reader);
        }
        const refused = await open("over-budget");
        assert(refused.isErr(), "admission never overcommits the budget");
        assert.match(inspect(refused.error), /receive capacity exhausted/);
        // Ending one transfer releases its window for the next one.
        await readers.pop()!.cancel();
        await runtime.waitFor(async () => {
          const admitted = await open("after-release");
          if (admitted.isErr()) return false;
          readers.push(
            (admitted.take() as ReadableStream<Uint8Array>).getReader(),
          );
          return true;
        });
      } finally {
        await Promise.all(readers.map((reader) => reader.cancel()));
        await client.connection.close();
        await service.stop();
        await service.wait();
      }
    });
  },
);
