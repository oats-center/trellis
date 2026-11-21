/** A finite RPC is never sent after its deadline, first send or resend. */
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, open, rm, type FileHandle } from "node:fs/promises";
import { closeSync, openSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { type BaseError } from "@oatscenter/result";
import { Result, TransportError } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const ECHO = apis.runtime.API.actions["rpc:Echo"].subject;

test("a request whose deadline passes before it is sent is never sent", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const gate = runtime.nativeTransportGate();
      const echoesSent = () =>
        gate
          .connections()
          .flatMap((connection) => connection.outboundContexts)
          .filter((publication) => publication.subject === ECHO).length;
      const identity = await runtime.registerService({
        name: "deadline-provider",
        contract: participants.Provider.participant,
      });
      const client = await runtime.connectClient({
        name: "deadline-caller",
        contract: apps.Caller.request,
      });
      const fifos = await mkdtemp(join(tmpdir(), "rpc-deadline-"));
      let stopService: (() => Promise<void>) | undefined;
      try {
        const assertTimedOut = (outcome: Result<unknown, BaseError>) => {
          assert(outcome.isErr());
          assert(
            outcome.error instanceof TransportError,
            String(outcome.error),
          );
          assert.equal(outcome.error.code, "trellis.request.timeout");
        };

        // With no provider the broker answers no-responders and the request is
        // resent after 100, 200, 400, and 800 ms; its last resend waits 1000 ms
        // and would go out at about 2.5 s. A caller whose event loop is busy
        // from 2 s until after its 3 s deadline wakes with that resend overdue.
        let started = Date.now();
        const resent = client.echo({ value: "resent" }, { timeout: 3_000 });
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        const sentBeforeStall = echoesSent();
        assert(sentBeforeStall >= 2, "the request was resending");
        while (Date.now() < started + 3_200);
        assertTimedOut(await resent);

        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          seed: identity.seed,
        }).orThrow();
        stopService = async () => {
          await service.stop();
          await service.wait();
        };
        const received: string[] = [];
        await service.handleEcho(({ input }) => {
          received.push(input.value);
          return Result.ok(input);
        });
        assert.deepEqual(await client.echo({ value: "ready" }).orThrow(), {
          value: "ready",
        });

        // Proof creation signs on the libuv thread pool. Occupy every pool
        // thread with a FIFO open that blocks until a writer arrives, so the
        // proof completes only after the deadline, as it does when blocking
        // file or crypto work saturates a real caller's pool.
        const poolSize = Number(process.env.UV_THREADPOOL_SIZE ?? 4);
        const readers: Promise<FileHandle>[] = [];
        for (let index = 0; index < poolSize; index++) {
          const fifo = join(fifos, String(index));
          execFileSync("mkfifo", [fifo]);
          readers.push(open(fifo, "r"));
        }
        started = Date.now();
        const late = client.echo({ value: "late" }, { timeout: 1_000 });
        await new Promise((resolve) =>
          setTimeout(resolve, started + 1_200 - Date.now()),
        );
        for (let index = 0; index < poolSize; index++)
          closeSync(openSync(join(fifos, String(index)), "w"));
        for (const reader of await Promise.all(readers)) await reader.close();
        assertTimedOut(await late);

        // A later request on the same connection is received and answered, so
        // anything the late request or the overdue resend had sent reached
        // the broker and provider before it.
        assert.deepEqual(await client.echo({ value: "normal" }).orThrow(), {
          value: "normal",
        });
        assert.deepEqual(
          { received, sentAfterDeadline: echoesSent() - sentBeforeStall - 2 },
          { received: ["ready", "normal"], sentAfterDeadline: 0 },
        );
      } finally {
        await rm(fifos, { recursive: true, force: true });
        await client.connection.close();
        await stopService?.();
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("a request in flight when its connection closes ends as closed and leaves nothing behind", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      await runtime.registerService({
        name: "closing-provider",
        contract: participants.Provider.participant,
      });
      const client = await runtime.connectClient({
        name: "closing-caller",
        contract: apps.Caller.request,
      });
      const gate = runtime.nativeTransportGate();
      // The caller is the only proxied participant; its admitted generation is
      // the open connection carrying subscriptions.
      const connection = await runtime.waitFor(
        () =>
          gate
            .connections()
            .filter((candidate) => !candidate.closed && candidate.subs.length)
            .at(-1) ?? false,
        { timeoutMs: 30_000 },
      );
      const fifos = await mkdtemp(join(tmpdir(), "rpc-closing-"));
      const unhandled: unknown[] = [];
      const record = (reason: unknown) => unhandled.push(reason);
      process.on("unhandledRejection", record);
      let closing: Promise<void> | undefined;
      try {
        // Stall proof creation on the libuv thread pool, as in the deadline
        // test above, so the request has its transport but has not subscribed
        // to its reply when the connection starts to close.
        const poolSize = Number(process.env.UV_THREADPOOL_SIZE ?? 4);
        const readers: Promise<FileHandle>[] = [];
        for (let index = 0; index < poolSize; index++) {
          const fifo = join(fifos, String(index));
          execFileSync("mkfifo", [fifo]);
          readers.push(open(fifo, "r"));
        }
        const started = Date.now();
        const request = client.echo({ value: "closing" }, { timeout: 1_500 });
        // Let the request acquire its transport and reach proof creation.
        await new Promise((resolve) => setTimeout(resolve, 200));
        // Closing drains the connection, which ends with a flush; withholding
        // the broker's answer keeps it draining while the proof completes.
        const drain = gate.holdNextPong(connection.id);
        closing = client.connection.close();
        await drain.held;
        for (let index = 0; index < poolSize; index++)
          closeSync(openSync(join(fifos, String(index)), "w"));
        for (const reader of await Promise.all(readers)) await reader.close();

        const outcome = await request;
        await drain.release();
        // Past the request's deadline nothing it started is still running: no
        // deadline timer fires and no settlement is left unhandled.
        await new Promise((resolve) =>
          setTimeout(resolve, started + 2_000 - Date.now()),
        );
        assert.deepEqual(unhandled.map(String), []);
        assert(outcome.isErr());
        assert(outcome.error instanceof TransportError, String(outcome.error));
        assert.equal(outcome.error.code, "trellis.request.closed");
      } finally {
        process.off("unhandledRejection", record);
        await rm(fifos, { recursive: true, force: true });
        await closing;
        await client.connection.close();
      }
    },
    { interruptibleNativeProxy: true },
  );
});
