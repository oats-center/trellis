/** Fresh application work started during a transport loss waits for the replacement and is sent once. */
import assert from "node:assert/strict";
import { Readable, Writable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { deadline } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("Rust fresh RPC started during loss is sent once on the replacement while its provider intake is still attaching", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const contract = participants.TransportGrowthSubject.participant;
      const targetContract = participants.TransportGrowthTarget.participant;
      await runtime.contracts.apply({
        deployment: "fresh-stage-target",
        contract: targetContract,
      });
      const targetInstance = await runtime.services.createInstance({
        deployment: "fresh-stage-target",
        name: "fresh-stage-target",
        contract: targetContract,
      });
      const target = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: targetContract,
        name: "fresh-stage-target",
        seed: targetInstance.seed,
      }).orThrow();
      let advances = 0;
      await target.handleAdvance(() => {
        advances++;
        return Result.ok({});
      });
      await target.handleExtend(() => Result.ok({}));
      await target.handleProgress(async ({ emit, signal }) => {
        while (!signal.aborted) {
          await emit({ value: "ready" });
          await new Promise((resolve) => setTimeout(resolve, 150));
        }
      });
      const targetExit = target.wait().catch(() => undefined);
      await runtime.contracts.apply({
        deployment: "fresh-stage-subject",
        contract,
      });
      const instance = await runtime.services.createInstance({
        deployment: "fresh-stage-subject",
        name: "fresh-stage-subject",
        contract,
      });
      const argv = rustFixtureArgv("transport_growth_subject");
      const child = spawnTracked(argv[0], argv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
          TRELLIS_IDENTITY_SEED: instance.seed,
        },
      });
      child.stderr.pipe(process.stderr);
      const stdin = Writable.toWeb(child.stdin).getWriter();
      const lines: string[] = [];
      let exited = false;
      const status = child.exited.then((value) => {
        exited = true;
        return value;
      });
      const drain = (async () => {
        let pending = "";
        for await (const chunk of Readable.toWeb(child.stdout).pipeThrough(
          new TextDecoderStream(),
        )) {
          const complete = (pending + chunk).split("\n");
          pending = complete.pop() ?? "";
          lines.push(...complete.map((line) => line.trim()));
        }
      })();
      const send = async (command: string) => {
        await stdin.write(new TextEncoder().encode(`${command}\n`));
      };
      const marker = async (prefix: string, count = 1) =>
        await runtime.waitFor(
          () => {
            const failure = lines.find((line) =>
              line.startsWith("TRANSPORT_GROWTH_ERROR "),
            );
            if (failure) throw new Error(failure);
            if (
              lines.filter((line) => line.startsWith(prefix)).length >= count
            ) {
              return true;
            }
            if (exited) throw new Error(lines.join("\n"));
          },
          { timeoutMs: 30_000, intervalMs: 20 },
        );
      const gate = runtime.nativeTransportGate();
      let readiness: ReturnType<typeof gate.armResponseHold> | undefined;
      try {
        await marker("TRANSPORT_GROWTH_ADVANCE_OK");
        assert.deepEqual(advances, 1);
        const original = gate
          .connections()
          .find((connection) =>
            connection.outboundContexts.some(
              (message) =>
                message.subject ===
                apis.transport_growth.API.actions["rpc:Advance"].subject,
            ),
          );
        assert(original);
        readiness = gate.armResponseHold(
          "$SYS.REQ.USER.INFO",
          undefined,
          (_body, connection) =>
            connection.id !== original.id &&
            connection.subs.some((sub) => sub.subject.endsWith(".Watch")),
        );
        runtime.interruptNativeTransport();
        runtime.restoreNativeTransport();
        const stageId = (await deadline(readiness.held, 60_000)).connectionId;
        assert.notEqual(stageId, original.id);
        const stage = gate.connection(stageId);
        assert(stage);
        assert(
          !stage.closed &&
            stage.subs.some((sub) => sub.subject.endsWith(".Watch")),
        );
        const before = stage.outboundContexts.length;
        // The replacement is published, but its provider intake's broker
        // readiness probe is withheld. Outgoing calls do not depend on that
        // intake: a call started now goes out on the replacement, once.
        await send("START_ADVANCE");
        await marker("TRANSPORT_GROWTH_ADVANCE_STARTED");
        await marker("TRANSPORT_GROWTH_ADVANCE_OK", 2);
        assert.deepEqual(advances, 2);
        assert.deepEqual(
          gate
            .connection(stageId)!
            .outboundContexts.slice(before)
            .filter((out) => out.subject.endsWith(".Advance")).length,
          1,
        );
        await readiness.release();
        await runtime.waitFor(async () => {
          const before = lines.length;
          await send("AUTH_STATUS");
          await runtime.waitFor(() =>
            lines.slice(before).find((line) => line.startsWith("AUTH_USABLE ")),
          );
          return lines.slice(before).includes("AUTH_USABLE true");
        });
        await send("ADVANCE");
        await marker("TRANSPORT_GROWTH_ADVANCE_OK", 3);
        assert.deepEqual(advances, 3);
        assert.deepEqual(
          gate
            .connection(stageId)!
            .outboundContexts.slice(before)
            .filter((out) => out.subject.endsWith(".Advance")).length,
          2,
        );
        await send("EXIT");
        await marker("TRANSPORT_GROWTH_DONE");
        await runtime.waitFor(() => (exited ? true : undefined));
        assert((await status).code === 0);
      } finally {
        await readiness?.release().catch(() => undefined);
        await stdin.close().catch(() => undefined);
        if (!exited && child.pid !== undefined) {
          try {
            child.kill("SIGKILL");
          } catch {
            /* already exited */
          }
        }
        await Promise.allSettled([status, drain]);
        await target.stop();
        await targetExit;
      }
    },
    {
      interruptibleNativeProxy: true,
      authorization: {
        contextLifetimeSeconds: 106,
      },
    },
  );
});
