import { PermissionViolationError } from "@nats-io/nats-core";
import { Result } from "@oatscenter/trellis";
import { connectNativeSession } from "@oatscenter/trellis/native";
import { TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { ulid } from "ulid";

import {
  apps as v1Apps,
  participants as v1Participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/v2/packages/runtime-trellis/index.js";
import { readRuntimeServiceAttachments } from "./_support/broker_inventory.ts";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

test(
  "a generated second-major application serves a signed Live session and graceful END",
  { timeout: 60_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const identity = await runtime.registerService({
        name: "live-v2-provider",
        contract: participants.LiveV2Provider.participant,
      });
      const provider = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: participants.LiveV2Provider.participant,
        seed: identity.seed,
      }).orThrow();
      try {
        await provider.handleWatch(async ({ input, emit }) => {
          await emit({ value: `v2:${input.value}` }).orThrow();
        });
        const caller = await runtime.connectClient({
          name: "live-v2-caller",
          contract: apps.LiveV2Caller.request,
        });
        const observation = await caller.watch({ value: "document" }).orThrow();
        const received = [];
        for await (const value of observation) received.push(value);
        assert.deepEqual(received, [{ value: "v2:document" }]);
      } finally {
        await provider.stop();
      }
    });
  },
);

test(
  "application majors isolate Live routes across Rust and Node, including dotted actions and authority renewal",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await using resources = new AsyncDisposableStack();
      const firstIdentity = await runtime.registerService({
        deployment: "live-v1",
        name: "live-v1-node",
        contract: v1Participants.LiveV1Provider.participant,
      });
      const first = await TrellisService.connect({
        trellisUrl: runtime.oauthIssuer,
        participant: v1Participants.LiveV1Provider.participant,
        seed: firstIdentity.seed,
      }).orThrow();
      resources.defer(() => first.stop());
      const released = Promise.withResolvers<void>();
      resources.defer(() => released.resolve());
      await first.handleEcho(({ input }) => Result.ok(input));
      await first.handleWatch(async ({ input, emit }) => {
        await emit({ value: `v1:${input.value}` }).orThrow();
        await released.promise;
        await emit({ value: `v1:released:${input.value}` }).orThrow();
      });
      await first.handleListWatch(async ({ input, emit }) => {
        await emit({ value: `v1:list:${input.value}` }).orThrow();
      });

      const secondIdentity = await runtime.registerService({
        deployment: "live-v2",
        name: "live-v2-rust",
        contract: participants.LiveV2Provider.participant,
      });
      const providerArgv = rustFixtureArgv("live_application_versions");
      const second = spawnTracked(providerArgv[0], providerArgv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_IDENTITY_SEED: secondIdentity.seed,
        },
      });
      let providerOutput = "";
      second.stdout.on("data", (chunk) => {
        providerOutput += String(chunk);
      });
      second.stderr.on("data", (chunk) => {
        providerOutput += String(chunk);
      });
      resources.defer(async () => {
        second.stdin.end();
        const status = await second.exited;
        assert.equal(status.code, 0, providerOutput);
      });
      const caller1 = await runtime.connectClient({
        name: "live-v1-node-caller",
        contract: v1Apps.LiveV1Caller.request,
      });
      const caller2 = await runtime.connectClient({
        name: "live-v2-node-caller",
        contract: apps.LiveV2Caller.request,
      });
      // A verified reply from the production router establishes broker-confirmed
      // intake before the case sends its single Live opening.
      await runtime.waitFor(async () => {
        const ready = await caller2.echo(
          { value: "ready" },
          { timeout: 1_000 },
        );
        return ready.isOk();
      });

      const view = await caller1.watch({ value: "document" }).orThrow();
      const iterator = view[Symbol.asyncIterator]();
      assert.deepEqual((await iterator.next()).value, { value: "v1:document" });
      await caller1.refreshAuthority();
      const [previousAttachment] = await readRuntimeServiceAttachments(
        runtime,
        participants.LiveV2Provider.participant.id,
      );
      assert(previousAttachment);
      await runtime.callAdminRpc("authConnectionsKick", {
        attachmentId: previousAttachment.attachmentId,
        idempotencyKey: ulid(),
      });
      await runtime.waitFor(
        async () => {
          const current = await readRuntimeServiceAttachments(
            runtime,
            participants.LiveV2Provider.participant.id,
          );
          if (
            !current.some(
              (item) => item.attachmentId !== previousAttachment.attachmentId,
            )
          )
            return false;
          return (
            await caller2.echo({ value: "reconnected" }, { timeout: 1_000 })
          ).isOk();
        },
        { timeoutMs: 30_000 },
      );
      released.resolve();
      assert.deepEqual((await iterator.next()).value, {
        value: "v1:released:document",
      });
      assert.equal((await iterator.next()).done, true);

      const secondValues = [];
      for await (const value of await caller2
        .watch({ value: "document" })
        .orThrow()) {
        secondValues.push(value);
      }
      assert.deepEqual(secondValues, [{ value: "v2:document" }]);
      const firstList = [];
      for await (const value of await caller1
        .listWatch({ value: "list" })
        .orThrow())
        firstList.push(value);
      assert.deepEqual(firstList, [{ value: "v1:list:list" }]);
      const secondList = [];
      for await (const value of await caller2
        .listWatch({ value: "list" })
        .orThrow())
        secondList.push(value);
      assert.deepEqual(secondList, [{ value: "v2:list:list" }]);
      const operation = await caller2
        .work({ value: "document" })
        .start()
        .orThrow();
      assert.deepEqual((await operation.wait().orThrow()).output, {
        value: "v2:work:document",
      });
      const terminal = [];
      for await (const event of await operation.live().orThrow()) {
        if (event.type === "completed") terminal.push(event.snapshot.output);
      }
      assert.deepEqual(terminal, [{ value: "v2:work:document" }]);

      const scoped = await connectNativeSession(
        await runtime.registerClient({
          name: "live-v1-broker-probe",
          contract: v1Apps.LiveV1Caller.request,
        }),
      ).orThrow();
      resources.defer(() => scoped.close());
      const socket = scoped.manager.currentNats();
      const forbidden = apis.documents.API.actions["live:Watch"].subject;
      const statuses = socket.status();
      let violation: PermissionViolationError | undefined;
      let statusError: unknown;
      const monitoring = (async () => {
        for await (const status of statuses) {
          if (
            status.type === "error" &&
            status.error instanceof PermissionViolationError
          ) {
            violation = status.error;
            return;
          }
        }
      })().catch((error: unknown) => {
        statusError = error;
      });
      socket.publish(forbidden, new Uint8Array());
      const rejected = await runtime.waitFor(() => {
        if (statusError) throw statusError;
        return violation;
      });
      await monitoring;
      assert.equal(rejected.operation, "publish");
      assert.equal(rejected.subject, forbidden);

      const callerArgv = rustFixtureArgv("live_application_versions", [
        "--caller",
      ]);
      const native = spawnTracked(callerArgv[0], callerArgv.slice(1), {
        env: {
          ...process.env,
          TRELLIS_URL: runtime.oauthIssuer,
          TRELLIS_NATIVE_CLIENT_ID: v1Apps.LiveV1Caller.request.id,
          XDG_CONFIG_HOME: join(runtime.workdir, "live-version-caller"),
        },
      });
      resources.defer(async () => {
        if (native.exitCode === null && native.signalCode === null)
          native.kill("SIGTERM");
        await native.exited;
      });
      let nativeOutput = "";
      let values: unknown;
      native.stderr.on("data", (chunk) => {
        nativeOutput += String(chunk);
      });
      for await (const line of createInterface({ input: native.stdout })) {
        nativeOutput += `${line}\n`;
        if (line.startsWith("NATIVE_LOGIN ")) {
          await authorizeRustNativeLogin(
            runtime,
            line,
            v1Apps.LiveV1Caller.request,
          );
        }
        if (line.startsWith("LIVE_VERSION_RESULT "))
          values = JSON.parse(line.slice("LIVE_VERSION_RESULT ".length));
      }
      assert.equal((await native.exited).code, 0, nativeOutput);
      assert.deepEqual(values, ["v1:rust", "v1:released:rust"]);
    });
  },
);
