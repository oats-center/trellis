import {
  credsAuthenticator,
  deadline,
  headers as natsHeaders,
} from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { isErr, Result } from "@oatscenter/trellis";
import { TransportError } from "@oatscenter/trellis/errors";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Readable, Writable } from "node:stream";
import { TextDecoderStream } from "node:stream/web";
import { test } from "node:test";
import { join } from "node:path";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-testkit/src/admin/methods.ts";
import { spawnTracked } from "./_support/process.ts";
import {
  authorizeRustNativeLogin,
  rustFixtureArgv,
  withTrellisRuntime,
} from "./_support/runtime.ts";

const persistedProgress = {
  value: "persisted",
  nested: {
    count: 9_007_199_254_740_993n,
    payload: Uint8Array.from([1, 2, 3]),
  },
};
const transientProgress = {
  value: "transient",
  nested: {
    count: 9_007_199_254_740_993n,
    payload: Uint8Array.from([4, 5, 6]),
  },
};

test("one principal can log out two distinct login sessions", async () => {
  await withTrellisRuntime(async (runtime) => {
    const first = await runtime.connectClient({
      name: "first-logout-session",
      contract: adminParticipant,
    });
    const second = await runtime.connectClient({
      name: "second-logout-session",
      contract: adminParticipant,
    });
    try {
      const firstSession = (await first.sessionsMe({}).orThrow()).login;
      const secondSession = (await second.sessionsMe({}).orThrow()).login;
      assert(firstSession && secondSession);
      assert.deepEqual(firstSession.principalId, secondSession.principalId);
      assert(firstSession.loginSessionId !== secondSession.loginSessionId);
      for (const client of [first, second]) {
        // Self-revocation still delivers its own reply.
        await client.sessionsLogout({}).orThrow();
        if (client === first) {
          assert.deepEqual(
            (await second.sessionsMe({}).orThrow()).login?.state,
            "active",
          );
        }
      }
      const sessions = await runtime.callAdminRpc("authSessionsList", {
        principalId: firstSession.principalId,
      });
      for (const session of [firstSession, secondSession]) {
        assert.deepEqual(
          sessions.items.find(
            (entry) => entry.loginSessionId === session.loginSessionId,
          )?.state,
          "revoked",
        );
      }
    } finally {
      await Promise.all([first.connection.close(), second.connection.close()]);
    }
  });
});

test("service bootstrap rejects a different issuer origin and admits the selected origin", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "bootstrap-origin-provider",
      contract: participants.Provider.participant,
    });
    const wrongUrl = new URL(runtime.trellisUrl);
    wrongUrl.hostname =
      wrongUrl.hostname === "localhost" ? "127.0.0.1" : "localhost";
    const result = await TrellisService.connect({
      trellisUrl: wrongUrl.origin,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).take();
    assert(isErr(result));

    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    await service.stop();
  });
});

test("service wait rejects an unrequested transport close but accepts stop", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "wait-close-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const exited = service.wait().catch((error: unknown) => error);
    await service.connection.close();
    const error = await exited;
    assert(error instanceof Error);
    assert(error.message.includes("closed unexpectedly"));

    const healthy = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const healthyExit = healthy.wait();
    await healthy.stop();
    await healthyExit;

    const stopped = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    stopped.jobs.work.handle(({ job }) =>
      Promise.resolve(Result.ok(job.payload)),
    );
    await stopped.stop();
    await stopped.wait();
    await stopped.wait();
  });
});

test("generated TypeScript caller reaches Rust provider", async () => {
  const providerArgv = rustFixtureArgv("trellis-runtime-acceptance");

  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "rust",
        contract: participants.OperationProvider.participant,
      });
      const startProvider = () => {
        const child = spawnTracked("setsid", providerArgv, {
          env: {
            ...process.env,
            TRELLIS_URL: runtime.trellisUrl,
            TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
            TRELLIS_IDENTITY_SEED: identity.seed,
            // Only the caller traverses the interruptible path in this journey.
            TRELLIS_NATS_URL: runtime.natsUrl,
          },
        });
        child.stdout.pipe(process.stdout);
        child.stderr.pipe(process.stderr);
        return child;
      };
      let providerProcess = startProvider();
      let exited = false;
      let status = providerProcess.exited.then((status) => {
        exited = true;
        return status;
      });
      try {
        const client = await runtime.connectClient({
          name: "cross-language",
          contract: apps.Caller.request,
        });
        const response = await runtime.waitFor(
          async () => {
            if (exited) {
              throw new Error(
                `Rust provider exited: ${JSON.stringify(await status)}`,
              );
            }
            const result = await client.echo(
              { value: "from TypeScript" },
              {
                timeout: 1000,
              },
            );
            return result.isOk() ? result.orThrow() : false;
          },
          { timeoutMs: 120_000 },
        );
        assert.deepEqual(response.value, "Rust received from TypeScript");

        const firstFeedAbort = new AbortController();
        const secondFeedAbort = new AbortController();
        const firstFeed = await client
          .runtimeWatch(
            {},
            {
              signal: firstFeedAbort.signal,
            },
          )
          .orThrow();
        const secondFeed = await client
          .runtimeWatch(
            {},
            {
              signal: secondFeedAbort.signal,
            },
          )
          .orThrow();
        const firstFeedIterator = firstFeed[Symbol.asyncIterator]();
        const secondFeedIterator = secondFeed[Symbol.asyncIterator]();
        assert(
          (await firstFeedIterator.next()).value?.value.startsWith(
            "rust-feed-",
          ),
        );
        assert(
          (await secondFeedIterator.next()).value?.value.startsWith(
            "rust-feed-",
          ),
        );
        firstFeedAbort.abort();
        assert(
          (await secondFeedIterator.next()).value?.value.startsWith(
            "rust-feed-",
          ),
        );
        secondFeedAbort.abort();

        const failed = await client
          .work({ value: "failure" })
          .start()
          .orThrow();
        assert.deepEqual((await failed.wait().orThrow()).state, "failed");

        const live = await client
          .work({ value: "reconnect-live" })
          .start()
          .orThrow();
        await runtime.waitFor(
          async () =>
            (await live.get().orThrow()).progress?.value === "persisted",
        );

        // Repeating the exact invocation must return the persisted typed snapshot
        // through the accepted path without executing the operation again.
        let replayedAccepted: { snapshot: { progress?: unknown } } | undefined;
        const replayed = await client
          .work({ value: "reconnect-live" })
          .start(
            {
              onAccepted: (event: { snapshot: { progress?: unknown } }) => {
                replayedAccepted = event;
              },
            },
            { invocationId: live.id },
          )
          .orThrow();
        assert.deepEqual(replayed.id, live.id);
        assert.deepEqual((await replayed.get().orThrow()).state, "running");
        assert.deepEqual(
          replayedAccepted?.snapshot.progress,
          persistedProgress,
        );

        runtime.interruptNativeTransport();
        try {
          await runtime.waitFor(() =>
            ["disconnected", "reconnecting"].includes(
              client.connection.status.phase,
            ),
          );
        } finally {
          runtime.restoreNativeTransport();
        }
        const reconnected = client;
        const resumedLive = reconnected.work.resume(live);
        await runtime.waitFor(async () => (await resumedLive.get()).isOk());
        assert.deepEqual(
          (await resumedLive.get().orThrow()).progress,
          persistedProgress,
        );
        const watched = (await resumedLive.live({ updates: true }).orThrow())[
          Symbol.asyncIterator
        ]();
        const initial = (await watched.next()).value;
        assert(initial?.type !== "update");
        assert.deepEqual(initial?.snapshot.progress, persistedProgress);

        await runtime.deployments.create({
          id: "operation-intruder",
          contract: participants.OperationCaller.participant,
        });
        const intruderIdentity = await runtime.registerService({
          name: "operation-intruder",
          contract: participants.OperationCaller.participant,
          deployment: "operation-intruder",
        });
        const intruder = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.OperationCaller.participant,
          seed: intruderIdentity.seed,
        }).orThrow();
        const intruderExit = intruder.wait().catch((error: unknown) => error);
        try {
          const intruderOperation = intruder.work.resume(live);
          assert((await intruderOperation.get()).isErr());
          await assert.rejects(async () => {
            const deniedWatch = await intruderOperation.live().orThrow();
            await deniedWatch[Symbol.asyncIterator]().next();
          });
          assert(
            (
              await intruderOperation.signal("Continue", {
                value: "intruder",
              })
            ).isErr(),
          );
          assert((await intruderOperation.cancel()).isErr());

          const signal = await resumedLive
            .signal("Continue", {
              value: "emit update",
            })
            .orThrow();
          let update = (await watched.next()).value;
          while (update?.type !== "update")
            update = (await watched.next()).value;
          assert(update?.type === "update");
          assert.deepEqual(
            (update.update as typeof transientProgress).value,
            "transient",
          );
          assert.deepEqual(update.update, transientProgress);
          const afterUpdate = await resumedLive.get().orThrow();
          assert(afterUpdate.revision >= signal.snapshot.revision);
          assert.deepEqual(afterUpdate.progress, signal.snapshot.progress);

          const late = (await resumedLive.live({ updates: true }).orThrow())[
            Symbol.asyncIterator
          ]();
          const lateInitial = (await late.next()).value;
          assert(lateInitial?.type !== "update");
          assert.deepEqual(lateInitial?.snapshot.progress, persistedProgress);
          await resumedLive.cancel().orThrow();
          let lateTerminal = (await late.next()).value;
          while (lateTerminal?.snapshot.state !== "cancelled") {
            assert(lateTerminal?.type !== "update");
            lateTerminal = (await late.next()).value;
          }
          assert.deepEqual(lateTerminal?.snapshot.state, "cancelled");
          let watchedTerminal = (await watched.next()).value;
          while (watchedTerminal?.snapshot.state !== "cancelled") {
            watchedTerminal = (await watched.next()).value;
          }
          assert.deepEqual(watchedTerminal.snapshot.state, "cancelled");
        } finally {
          await intruder.stop();
          assert.deepEqual(await intruderExit, undefined);
        }

        const resumable = await reconnected
          .work({ value: "resume" })
          .start()
          .orThrow();
        await runtime.waitFor(
          async () =>
            (await resumable.get().orThrow()).progress?.value === "persisted",
        );
        const bytes = Uint8Array.from(
          { length: 131_073 },
          (_, index) => index % 251,
        );
        const transferred = await reconnected
          .upload({ value: "upload" })
          .transfer(bytes)
          .start()
          .orThrow();
        await runtime.waitFor(
          async () =>
            (await transferred.operation.get().orThrow()).transfer
              ?.transferredBytes === bytes.length,
        );
        if (providerProcess.pid !== undefined)
          process.kill(-providerProcess.pid, "SIGKILL");
        await status;
        exited = false;
        providerProcess = startProvider();
        status = providerProcess.exited.then((value) => {
          exited = true;
          return value;
        });
        await runtime.waitFor(
          async () =>
            (
              await reconnected.echo(
                { value: "from TypeScript" },
                {
                  timeout: 1000,
                },
              )
            ).isOk(),
          { timeoutMs: 120_000 },
        );
        const resumed = await runtime.waitFor(
          async () => {
            const snapshot = await resumable.get().orThrow();
            return snapshot.state === "completed" ? snapshot : false;
          },
          { timeoutMs: 120_000 },
        );
        assert.deepEqual(resumed.state, "completed");
        assert.deepEqual(resumed.output?.value, "resumed");

        const uploaded = await runtime.waitFor(
          async () => {
            const snapshot = await transferred.operation.get().orThrow();
            return snapshot.state === "completed" ? snapshot : false;
          },
          { timeoutMs: 120_000 },
        );
        assert.deepEqual(uploaded.state, "completed");
        assert.deepEqual(uploaded.output?.value, `upload:${bytes.length}:true`);

        // Generated Rust caller leg: the same running Rust provider serves the
        // generated Rust Feed and Operation calls.
        const callerArgv = rustFixtureArgv("caller");
        const callerChild = spawnTracked(callerArgv[0], callerArgv.slice(1), {
          env: {
            ...process.env,
            TRELLIS_URL: runtime.trellisUrl,
            TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
            XDG_CONFIG_HOME: join(runtime.workdir, "rust-caller-config"),
          },
        });
        callerChild.stderr.pipe(process.stderr);
        const callerReader = Readable.toWeb(callerChild.stdout)
          .pipeThrough(new TextDecoderStream())
          .getReader();
        let callerOutput = "";
        while (!/^NATIVE_LOGIN .+\n/m.test(callerOutput)) {
          const chunk = await callerReader.read();
          assert(!chunk.done, callerOutput);
          callerOutput += chunk.value;
        }
        await authorizeRustNativeLogin(
          runtime,
          callerOutput,
          apps.Caller.request,
        );
        while (!callerOutput.includes("rust caller complete")) {
          const chunk = await callerReader.read();
          assert(!chunk.done, callerOutput);
          callerOutput += chunk.value;
        }
        assert.equal((await callerChild.exited).code, 0, callerOutput);
      } finally {
        if (!exited && providerProcess.pid !== undefined)
          process.kill(-providerProcess.pid, "SIGKILL");
        await status;
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("simultaneous Feed controls remain creator and feed scoped", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "feed-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let cancelled = 0;
    let nextFeed = 0;
    await service.handleWatch(async ({ emit, signal }) => {
      const feed = ++nextFeed;
      let frame = 0;
      while (!signal.aborted) {
        await emit({ value: `feed-${feed}-${++frame}` }).orThrow();
        await new Promise((resolve) => setTimeout(resolve, 25));
      }
      cancelled += 1;
    });
    const serviceExit = service.wait();
    const caller = await runtime.connectClient({
      name: "feed-caller",
      contract: apps.Caller.request,
    });
    const firstAbort = new AbortController();
    const secondAbort = new AbortController();
    try {
      const first = await caller
        .runtimeWatch({}, { signal: firstAbort.signal })
        .orThrow();
      const second = await caller
        .runtimeWatch({}, { signal: secondAbort.signal })
        .orThrow();
      const firstIterator = first[Symbol.asyncIterator]();
      const secondIterator = second[Symbol.asyncIterator]();
      assert((await firstIterator.next()).value?.value.startsWith("feed-1-"));
      assert((await secondIterator.next()).value?.value.startsWith("feed-2-"));

      firstAbort.abort();
      await runtime.waitFor(() => cancelled === 1);
      assert((await secondIterator.next()).value?.value.startsWith("feed-2-"));
      assert.deepEqual(cancelled, 1);
    } finally {
      firstAbort.abort();
      secondAbort.abort();
      await runtime.waitFor(() => cancelled === 2);
      await caller.connection.close();
      await service.stop();
      await serviceExit;
    }
  });
});

test("generated Rust resources use live NATS", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust-resources",
      contract: participants.Provider.participant,
    });
    const resourcesArgv = rustFixtureArgv("resources");
    const child = spawnTracked(resourcesArgv[0], resourcesArgv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_IDENTITY_SEED: identity.seed,
      },
    });
    child.stderr.pipe(process.stderr);
    const reader = Readable.toWeb(child.stdout)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    let output = "";
    while (!output.includes("rust resources ready")) {
      const chunk = await reader.read();
      assert(!chunk.done, output);
      output += chunk.value;
    }
    const stdin = Writable.toWeb(child.stdin).getWriter();
    try {
      await runtime.contracts.apply({
        contract: removedParticipants.Provider.participant,
      });
    } finally {
      await stdin.write(new TextEncoder().encode("replacement complete\n"));
      await stdin.close();
    }

    while (!output.includes("rust resources invalidated")) {
      const chunk = await Promise.race([
        reader.read(),
        new Promise<never>((_, reject) =>
          setTimeout(
            () =>
              reject(new Error(`resource invalidation timed out: ${output}`)),
            60_000,
          ),
        ),
      ]);
      assert(!chunk.done, output);
      output += chunk.value;
    }
    const status = await child.exited;
    assert.equal(status.code, 0);

    const stateArgv = rustFixtureArgv("resources");
    const stateChild = spawnTracked(stateArgv[0], stateArgv.slice(1), {
      env: {
        ...process.env,
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
        TRELLIS_STATE_ACCEPTANCE: "1",
        XDG_CONFIG_HOME: join(runtime.workdir, "rust-state-config"),
      },
    });
    stateChild.stderr.pipe(process.stderr);
    const stateReader = Readable.toWeb(stateChild.stdout)
      .pipeThrough(new TextDecoderStream())
      .getReader();
    let stateOutput = "";
    while (!/^NATIVE_LOGIN .+\n/m.test(stateOutput)) {
      const chunk = await stateReader.read();
      assert(!chunk.done, stateOutput);
      stateOutput += chunk.value;
    }
    await authorizeRustNativeLogin(
      runtime,
      stateOutput,
      apps.NativeStateCaller.request,
    );
    while (!stateOutput.includes("rust state complete")) {
      const chunk = await stateReader.read();
      assert(!chunk.done, stateOutput);
      stateOutput += chunk.value;
    }
    assert.equal((await stateChild.exited).code, 0);
  });
});

test("generated runtime workflows", async (t) => {
  await withTrellisRuntime(
    async (runtime) => {
      const identity = await runtime.registerService({
        name: "provider",
        contract: participants.Provider.participant,
      });
      const wrongSeed = `${identity.seed.slice(0, 5)}${
        identity.seed[5] === "A" ? "B" : "A"
      }${identity.seed.slice(6)}`;
      assert(
        (
          await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.Provider.participant,
            seed: wrongSeed,
          })
        ).isErr(),
      );
      assert(
        (
          await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.AdminService.participant,
            seed: identity.seed,
          })
        ).isErr(),
      );
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: "provider",
        seed: identity.seed,
      }).orThrow();
      let cancelledFeeds = 0;
      let nextFeedId = 0;
      const cancellationObserved = Promise.withResolvers<void>();
      const lateCompletionRejected = Promise.withResolvers<boolean>();
      const cancellationCleanup = Promise.withResolvers<void>();
      let serviceExit: Promise<unknown> | undefined;
      try {
        await service.handleEcho(({ input }) => Result.ok(input));
        const workHandler: Parameters<typeof service.handleWork>[0] = async ({
          input,
          op,
          signal,
        }) => {
          if (signal.aborted) return;
          await op.started().orThrow();
          if (input.value === "reconnect-live") {
            const accepted = await op.nextSignal("Continue").orThrow();
            await op.emitUpdate(transientProgress).orThrow();
            await op.acknowledgeSignal(accepted.sequence).orThrow();
            return op.defer();
          }
          if (input.value === "cancel-active") {
            if (!signal.aborted) {
              await new Promise<void>((resolve) => {
                signal.addEventListener("abort", () => resolve(), {
                  once: true,
                });
              });
            }
            cancellationObserved.resolve();
            lateCompletionRejected.resolve(
              (await op.complete({ value: "too late" })).isErr(),
            );
            await cancellationCleanup.promise;
            return;
          }
          return await op
            .complete({ value: `completed ${input.value}` })
            .orThrow();
        };
        await service.handleWork(workHandler);
        let attempts = 0;
        service.jobs.work.handle(({ job }) =>
          Promise.resolve(
            ++attempts === 1
              ? Result.err(new RetryJobError())
              : Result.ok(job.payload),
          ),
        );
        let releaseFirstKeyed!: () => void;
        const firstKeyedRelease = new Promise<void>((resolve) => {
          releaseFirstKeyed = resolve;
        });
        const keyedRuns = new Map<string, number>();
        service.jobs.keyedWork.handle(
          async ({ job }) => {
            keyedRuns.set(
              job.payload.value,
              (keyedRuns.get(job.payload.value) ?? 0) + 1,
            );
            if (job.payload.value === "first") await firstKeyedRelease;
            return Result.ok(job.payload);
          },
          { concurrency: 2 },
        );
        await service.handleWatch(async ({ emit, signal }) => {
          const feedId = ++nextFeedId;
          let frame = 0;
          while (!signal.aborted) {
            await emit({ value: `feed-${feedId}-${++frame}` }).orThrow();
            await new Promise((resolve) => setTimeout(resolve, 25));
          }
          cancelledFeeds += 1;
        });
        await service.handleUpload(async ({ input, op, transfer }) => {
          assert(transfer);
          const body = await transfer.stream().orThrow();
          const store = await service.store.files.open().orThrow();
          await store.put(input.value, body).orThrow();
          return await op.complete(input).orThrow();
        });
        let received: string | undefined;
        await service
          .onChanged(({ event }) => {
            received = event.value;
            return Result.ok(undefined);
          })
          .orThrow();
        serviceExit = service.wait().catch((error: unknown) => error);
        const client = await runtime.connectClient({
          name: "caller",
          contract: apps.Caller.request,
        });
        await t.test("operation executes and completes", async () => {
          const operation = await client
            .work({ value: "work" })
            .start()
            .orThrow();
          const terminal = await operation.wait().orThrow();
          assert.deepEqual(terminal.state, "completed");
          assert.deepEqual(terminal.output?.value, "completed work");
          const rejected = await client
            .work({ value: "different" })
            .start(undefined, { invocationId: operation.id })
            .take();
          assert(isErr(rejected));
          assert(rejected.error instanceof TransportError);
          assert.deepEqual(
            rejected.error.code,
            "trellis.operation.remote_error",
          );
          assert(
            rejected.error.message.includes(
              "Invocation id was already accepted with different input",
            ),
          );
        });
        await t.test(
          "cancellation aborts and fences the active handler",
          async () => {
            const operation = await client
              .work({ value: "cancel-active" })
              .start()
              .orThrow();
            await runtime.waitFor(
              async () => (await operation.get().orThrow()).state === "running",
            );
            const cancellation = operation.cancel().orThrow();
            await deadline(cancellationObserved.promise, 10_000);
            assert(await deadline(lateCompletionRejected.promise, 10_000));
            assert.deepEqual(
              (await operation.get().orThrow()).state,
              "running",
            );
            cancellationCleanup.resolve();
            assert.deepEqual((await cancellation).state, "cancelled");
            assert.deepEqual(
              (await operation.get().orThrow()).state,
              "cancelled",
            );
          },
        );
        await t.test(
          "the same native credential controls its operation after transport interruption",
          async () => {
            const origin = await runtime.connectClient({
              name: "operation-reconnect-origin",
              contract: apps.Caller.request,
            });
            try {
              const operation = await origin
                .work({ value: "reconnect-live" })
                .start()
                .orThrow();
              await runtime.waitFor(
                async () =>
                  (await operation.get().orThrow()).state === "running",
              );
              // A fresh Device Code login has another DPoP binding, not the
              // original operation's owner. Interrupt transport while retaining
              // ordinary native custody and let the connection recover itself.
              runtime.interruptNativeTransport();
              try {
                await runtime.waitFor(() =>
                  ["disconnected", "reconnecting"].includes(
                    origin.connection.status.phase,
                  ),
                );
              } finally {
                runtime.restoreNativeTransport();
              }
              const reconnected = origin;
              try {
                const resumed = reconnected.work.resume(operation);
                await runtime.waitFor(async () => {
                  const snapshot = await resumed.get();
                  if (snapshot.isOk()) {
                    assert.notEqual(
                      snapshot.orThrow().state,
                      "failed",
                      "transport interruption must not become a business failure",
                    );
                  }
                  return (
                    snapshot.isOk() && snapshot.orThrow().state === "running"
                  );
                });
                const sibling = await runtime.services.createInstance({
                  name: "provider-sibling",
                  contract: participants.Provider.participant,
                });
                assert.deepEqual(sibling.deploymentId, identity.deploymentId);
                const siblingService = await TrellisService.connect({
                  trellisUrl: runtime.trellisUrl,
                  participant: participants.Provider.participant,
                  name: "provider-sibling",
                  seed: sibling.seed,
                }).orThrow();
                const siblingExit = siblingService
                  .wait()
                  .catch((error) => error);
                await siblingService.handleWork(workHandler);
                try {
                  const watches = await Promise.all([
                    resumed.live({ updates: true }).orThrow(),
                    resumed.live({ updates: true }).orThrow(),
                  ]).then((streams) =>
                    streams.map((stream) => stream[Symbol.asyncIterator]()),
                  );
                  try {
                    for (const watch of watches) {
                      assert.deepEqual(
                        (await watch.next()).value?.snapshot.state,
                        "running",
                      );
                    }
                    const nats = await connect({
                      servers: runtime.natsUrl,
                      authenticator: credsAuthenticator(
                        await readFile(
                          join(
                            runtime.workdir,
                            "config/trellis/nats/creds/trellis-auth.creds",
                          ),
                        ),
                      ),
                    });
                    try {
                      const encode = (value: string) =>
                        Buffer.from(value).toString("base64url");
                      const subject = `operation.v1.${encode(
                        "runtime-trellis.runtime@v1",
                      )}.${encode(
                        identity.deploymentId,
                      )}.Work.updates.${operation.id}`;
                      const forged = natsHeaders();
                      forged.set("trellis-owner-executor", identity.instanceId);
                      forged.set("trellis-owner-epoch", "1");
                      forged.set("trellis-update-sequence", "1");
                      forged.set(
                        "trellis-update-time",
                        new Date().toISOString(),
                      );
                      nats.publish(
                        subject,
                        JSON.stringify({ value: "forged" }),
                        {
                          headers: forged,
                          reply: `${subject}.owner.${identity.instanceId}`,
                        },
                      );
                      await nats.flush();
                    } finally {
                      await nats.close();
                    }
                    await resumed
                      .signal("Continue", { value: "continue" })
                      .orThrow();
                    for (const watch of watches) {
                      let update = (await watch.next()).value;
                      while (update && update.type !== "update") {
                        update = (await watch.next()).value;
                      }
                      assert(
                        update?.type === "update",
                        `observation ended before an executor update: ${JSON.stringify(await resumed.get().orThrow())}`,
                      );
                      assert.deepEqual(update.update, transientProgress);
                    }
                  } finally {
                    for (const watch of watches) await watch.return?.();
                  }
                } finally {
                  await siblingService.stop();
                  assert.deepEqual(await siblingExit, undefined);
                }
                const late = (await resumed.live({ updates: true }).orThrow())[
                  Symbol.asyncIterator
                ]();
                try {
                  assert((await late.next()).value?.type !== "update");
                  const cancel = await resumed.cancel();
                  if (cancel.isErr()) {
                    throw new Error(
                      `reconnected cancel failed: ${JSON.stringify(cancel.error)}`,
                    );
                  }
                  assert.deepEqual(cancel.orThrow().state, "cancelled");
                } finally {
                  await late.return?.();
                }
              } finally {
                await reconnected.connection.close();
              }
            } finally {
              await origin.connection.close();
            }
          },
        );
        await t.test("event reaches generated subscriber", async () => {
          await service.publishChanged({ value: "published" }).orThrow();
          assert.deepEqual(await runtime.waitFor(() => received), "published");
        });
        await t.test("job retries then completes", async () => {
          const startedAt = Date.now();
          const job = await service.jobs.work
            .create({ value: "retried" })
            .orThrow();
          const initial = await job.get().orThrow();
          assert(initial.deadline);
          assert.deepEqual(
            Date.parse(initial.deadline) - Date.parse(initial.createdAt),
            5_000,
          );
          const terminal = await job.wait().orThrow();
          assert.deepEqual(terminal.state, "completed");
          assert.deepEqual(terminal.result, { value: "retried" });
          assert.deepEqual(attempts, 2);
          assert(Date.now() - startedAt >= 200);
        });
        await t.test("keyed wait does not consume deliveries", async () => {
          const first = await service.jobs.keyedWork
            .create({
              key: "shared",
              value: "first",
            })
            .orThrow();
          await runtime.waitFor(
            async () => (await first.get().orThrow()).state === "active",
          );
          const second = await service.jobs.keyedWork
            .create({
              key: "shared",
              value: "second",
            })
            .orThrow();
          const horizon = Date.now() + 26_000;
          await runtime.waitFor(
            async () => {
              const snapshot = await second.get().orThrow();
              assert.deepEqual(snapshot.tries, 0);
              assert.deepEqual(snapshot.state, "pending");
              return Date.now() >= horizon;
            },
            { timeoutMs: 30_000 },
          );
          assert.deepEqual(keyedRuns.get("second"), undefined);

          releaseFirstKeyed();
          assert.deepEqual((await first.wait().orThrow()).state, "completed");
          assert.deepEqual((await second.wait().orThrow()).state, "completed");
          assert.deepEqual(
            keyedRuns,
            new Map([
              ["first", 1],
              ["second", 1],
            ]),
          );
        });
        await t.test(
          "Jobs Summary matches filtered Query across pages",
          async () => {
            const console = await runtime.connectClient({
              name: "jobs-summary-console",
              contract: apps.JobsAdmin.request,
            });
            const observer = await runtime.connectClient({
              name: "jobs-summary-observer",
              contract: apps.Observer.request,
            });
            try {
              for (let index = 0; index < 3; index += 1) {
                const job = await service.jobs.work
                  .create({
                    value: `summary-parity-${index}`,
                  })
                  .orThrow();
                assert.deepEqual(
                  (await job.wait().orThrow()).state,
                  "completed",
                );
              }
              const filter = {
                state: ["completed" as const],
                groupBy: "state" as const,
              };
              const first = await runtime.waitFor(async () => {
                const page = await console
                  .query({
                    ...filter,
                    page: { limit: 1 },
                  })
                  .orThrow();
                return page.items.length === 1 && page.page.nextCursor
                  ? page
                  : false;
              });
              const second = await console
                .query({
                  ...filter,
                  page: { limit: 1, cursor: first.page.nextCursor },
                })
                .orThrow();
              const summary = await observer.jobsSummary(filter).orThrow();

              assert.deepEqual(first.items.length, 1);
              assert.deepEqual(second.items.length, 1);
              assert.deepEqual(summary.count, 6n);
              assert.deepEqual(summary.stats.total, 6n);
              assert.deepEqual(summary.stats.byState, { completed: 6n });
              assert.deepEqual(
                summary.groups.map(({ state, count }) => ({ state, count })),
                [
                  {
                    state: "completed",
                    count: 6n,
                  },
                ],
              );
            } finally {
              await console.connection.close();
              await observer.connection.close();
            }
          },
        );
        await t.test("transfer preserves exact bytes", async () => {
          const bytes = Uint8Array.from(
            { length: 131073 },
            (_, index) => index % 251,
          );
          const operation = await client
            .upload({ value: "bytes" })
            .transfer(bytes)
            .start()
            .orThrow();
          const terminal = await operation.wait().orThrow();
          assert.deepEqual(terminal.terminal.state, "completed");
          assert.deepEqual(terminal.transferred.size, bytes.length);
          const entry = await service.store.files.waitFor("bytes").orThrow();
          assert.deepEqual(await entry.bytes().orThrow(), bytes);
        });
        await t.test(
          "feed controls are isolated per simultaneous feed",
          async () => {
            const firstAbort = new AbortController();
            const secondAbort = new AbortController();
            try {
              const first = await client
                .runtimeWatch(
                  {},
                  {
                    signal: AbortSignal.any([
                      firstAbort.signal,
                      AbortSignal.timeout(10_000),
                    ]),
                  },
                )
                .orThrow();
              const second = await client
                .runtimeWatch(
                  {},
                  {
                    signal: AbortSignal.any([
                      secondAbort.signal,
                      AbortSignal.timeout(10_000),
                    ]),
                  },
                )
                .orThrow();
              const firstIterator = first[Symbol.asyncIterator]();
              const secondIterator = second[Symbol.asyncIterator]();
              assert(
                (await firstIterator.next()).value?.value.startsWith("feed-1-"),
              );
              assert(
                (await secondIterator.next()).value?.value.startsWith(
                  "feed-2-",
                ),
              );

              firstAbort.abort();
              await runtime.waitFor(() => cancelledFeeds === 1);
              assert(
                (await secondIterator.next()).value?.value.startsWith(
                  "feed-2-",
                ),
              );
              assert.deepEqual(cancelledFeeds, 1);
            } finally {
              firstAbort.abort();
              secondAbort.abort();
            }
            await runtime.waitFor(() => cancelledFeeds === 2);
          },
        );
        await t.test("state survives control-plane restart", async () => {
          await client.state.saved.set({ value: "durable" }).orThrow();
          await runtime.restart();
          await runtime
            .waitFor(
              () =>
                client.connection.status.phase === "connected" &&
                service.connection.status.phase === "connected",
            )
            .catch((cause) => {
              throw new Error(
                `Restart recovery failed: ${JSON.stringify({
                  caller: client.connection.status,
                  service: service.connection.status,
                })}`,
                { cause },
              );
            });
          const stored = await client.state.saved.get().orThrow();
          assert(stored);
          assert.deepEqual(stored.value.value, "durable");
        });
        await t.test(
          "an administrator-deployed service administers and login revocation fences the caller",
          async () => {
            const identity = await runtime.registerService({
              name: "nonadmin-service",
              deployment: "nonadmin-service",
              contract: participants.AdminService.participant,
            });
            const native = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.AdminService.participant,
              seed: identity.seed,
            }).orThrow();
            try {
              // A platform manager deployed it with administrative capabilities.
              assert(
                (await native.deploymentsList({})).isOk(),
                "an approved administrative service holds its administration",
              );
              assert.equal(
                (await client.echo({ value: "allowed" }).orThrow()).value,
                "allowed",
              );
              const sessions = await runtime.callAdminRpc(
                "authAuthorizationSessionsList",
                { clientId: apps.Caller.request.id, state: "active" },
              );
              const logins = new Set(
                sessions.items
                  .map((session) => session.loginSessionId)
                  .filter((id) => id !== undefined),
              );
              assert(
                logins.size > 0,
                "the admitted native caller must have an OAuth login",
              );
              for (const loginSessionId of logins)
                await runtime.callAdminRpc("authSessionsRevoke", {
                  loginSessionId,
                  idempotencyKey: ulid(),
                });
              await runtime.waitFor(async () =>
                (
                  await client.echo({ value: "revoked" }, { timeout: 1000 })
                ).isErr(),
              );
            } finally {
              await native.stop();
            }
          },
        );
      } finally {
        cancellationCleanup.resolve();
        await service.stop();
        const error = await serviceExit;
        assert.deepEqual(error, undefined);
      }
    },
    { interruptibleNativeProxy: true },
  );
});

test("surviving replica reclaims an expired operation lease", async () => {
  await withTrellisRuntime(async (runtime) => {
    const first = await runtime.registerService({
      name: "operation-replicas",
      contract: participants.Provider.participant,
    });
    const identities = [
      first,
      await runtime.services.createInstance({
        name: "operation-replica-b",
        contract: participants.Provider.participant,
      }),
    ];
    const services = await Promise.all(
      identities.map((identity) =>
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.Provider.participant,
          name: "operation-replicas",
          seed: identity.seed,
        }).orThrow(),
      ),
    );
    const started = Promise.withResolvers<number>();
    let executions = 0;
    for (const [index, service] of services.entries()) {
      await service.handleWork(async ({ input, op }) => {
        await op.started().orThrow();
        executions += 1;
        if (executions === 1) {
          started.resolve(index);
          return await new Promise<never>(() => {});
        }
        return await op
          .complete({ value: `reclaimed ${input.value}` })
          .orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-failover-caller",
      contract: apps.Caller.request,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client.work({ value: "lease" }).start().orThrow();
      const owner = await started.promise;
      await services[owner].stop();
      await exits[owner];
      const terminal = await operation.wait().orThrow();
      assert.deepEqual(terminal.state, "completed");
      assert.deepEqual(terminal.output?.value, "reclaimed lease");
      assert.deepEqual(executions, 2);
    } finally {
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(exits);
    }
  });
});

test("parallel client and service shutdown is repeatable", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "parallel-shutdown-provider",
      contract: participants.Provider.participant,
    });

    for (let round = 0; round < 3; round++) {
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name: `parallel-shutdown-${round}`,
        seed: identity.seed,
      }).orThrow();
      const exit = service.wait();
      const clients = [];
      for (let index = 0; index < 3; index++) {
        clients.push(
          await runtime.connectClient({
            name: `parallel-shutdown-${round}-${index}`,
            contract: apps.Caller.request,
          }),
        );
      }

      await Promise.all([
        service.stop(),
        ...clients.map((client) => client.connection.close()),
      ]);
      await exit;
    }
  });
});

test("ordinary attempts stay bounded while failed cleanup redelivers beyond that budget", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "retry-reconciliation-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    let ordinary = 0;
    let cleanup = 0;
    service.jobs.work.handle(({ job }) => {
      if (job.cancellationReason === "retry-exhausted") {
        cleanup += 1;
        return Promise.resolve(
          cleanup < 3
            ? Result.err(new RetryJobError())
            : Result.ok(job.payload),
        );
      }
      assert.deepEqual(job.cancelled, false);
      ordinary += 1;
      return Promise.resolve(Result.err(new RetryJobError()));
    });
    const exit = service.wait();
    try {
      const job = await service.jobs.work
        .create({
          value: "bounded work, durable cleanup",
        })
        .orThrow();
      const terminal = await job.wait().orThrow();
      assert.deepEqual(terminal.state, "dead");
      assert.deepEqual(ordinary, 2);
      assert.deepEqual(cleanup, 3);
    } finally {
      await service.stop();
      await exit;
    }
  });
});

test("started job deadline waits for native handler reconciliation", async () => {
  await withTrellisRuntime(async (runtime) => {
    const admin = await runtime.connectClient({
      name: "deadline-reconciliation-admin",
      contract: adminParticipant,
    });
    const identity = await runtime.registerService({
      name: "deadline-reconciliation-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    let deadlineObserved = false;
    let cleanupReturned = false;
    let starts = 0;
    service.jobs.work.handle(
      async ({ job }) => {
        starts += 1;
        entered.resolve();
        if (!job.signal.aborted) {
          await new Promise<void>((resolve) => {
            job.signal.addEventListener("abort", () => resolve(), {
              once: true,
            });
          });
        }
        assert.deepEqual(job.cancellationReason, "deadline-exceeded");
        if (job.payload.value === "deadline barrier") {
          return Result.ok(job.payload);
        }
        deadlineObserved = true;
        await release.promise;
        cleanupReturned = true;
        return Result.ok(job.payload);
      },
      { concurrency: 2 },
    );
    const exit = service.wait();
    try {
      const job = await service.jobs.work
        .create({
          value: "owned until cleanup",
        })
        .orThrow();
      await runtime.waitFor(() => starts === 1);
      await entered.promise;
      await runtime.waitFor(() =>
        job
          .get()
          .orThrow()
          .then((snapshot) => snapshot.state === "active"),
      );
      await runtime.waitFor(() => deadlineObserved, { timeoutMs: 12_000 });
      const barrier = await service.jobs.work
        .create({
          value: "deadline barrier",
        })
        .orThrow();
      const barrierResult = await deadline(
        barrier.wait().orThrow(),
        12_000,
      ).catch(async (cause: unknown) => {
        throw new Error(
          `deadline barrier did not settle: ${JSON.stringify(await barrier.get().orThrow())}`,
          { cause },
        );
      });
      assert.deepEqual(barrierResult.state, "expired");
      assert.deepEqual((await job.get().orThrow()).state, "active");
      const projection = await admin.jobsQuery({}).orThrow();
      assert.deepEqual(
        projection.items.find((item) => item.id === job.id)?.state,
        "active",
      );
      assert.deepEqual(cleanupReturned, false);
      release.resolve();
      const terminal = await deadline(job.wait().orThrow(), 12_000);
      assert.deepEqual(terminal.state, "expired");
      assert.deepEqual(cleanupReturned, true);
      assert.deepEqual(starts, 2);
    } finally {
      release.resolve();
      await service.stop();
      await exit;
    }
  });
});

test("unkeyed job maintains progress across broker ACK windows and completes once", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "unkeyed-progress-provider",
      contract: participants.Provider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: identity.seed,
    }).orThrow();
    const entered = Promise.withResolvers<void>();
    const release = Promise.withResolvers<void>();
    let starts = 0;
    service.jobs.work.handle(
      async ({ job }) => {
        starts += 1;
        entered.resolve();
        await release.promise;
        return Result.ok(job.payload);
      },
      { concurrency: 2 },
    );
    const exit = service.wait();
    try {
      const payload = { value: "blocked across ACK windows" };
      const job = await service.jobs.work.create(payload).orThrow();
      await runtime.waitFor(() => starts > 0);
      await entered.promise;
      await runtime.waitFor(
        async () => (await job.get().orThrow()).state === "active",
      );
      const startedAt = performance.now();
      // Three initial 250ms ACK windows, with a second worker able to redeliver.
      const held = await runtime.waitFor(
        async () => {
          const snapshot = await job.get().orThrow();
          if (
            snapshot.state !== "active" ||
            snapshot.tries !== 1 ||
            starts !== 1
          )
            return snapshot;
          return performance.now() - startedAt >= 750 ? snapshot : false;
        },
        { timeoutMs: 3_000 },
      );
      assert.deepEqual(held.state, "active");
      assert.deepEqual(held.tries, 1);
      assert.deepEqual(starts, 1);
      assert(performance.now() - startedAt >= 750);

      release.resolve();
      const terminal = await job.wait().orThrow();
      assert.deepEqual(terminal.state, "completed");
      assert.deepEqual(terminal.result, payload);
      assert.deepEqual(terminal.tries, 1);
      assert.deepEqual(starts, 1);
    } finally {
      release.resolve();
      await service.stop();
      await exit;
    }
  });
});
