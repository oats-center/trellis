import { jetstreamManager } from "@nats-io/jetstream";
import { credsAuthenticator, headers as natsHeaders } from "@nats-io/nats-core";
import { connect } from "@nats-io/transport-node";
import { Result } from "@oatscenter/trellis";
import { TransportError } from "@oatscenter/trellis/errors";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { assert, assertEquals, assertRejects } from "@std/assert";
import { fromFileUrl, join } from "@std/path";
import { participants as webParticipants } from "trellis-web-generated";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { participants as removedParticipants } from "../../integration/fixtures/runtime-removed/packages/runtime-trellis/index.js";
import { adminParticipant } from "../packages/trellis-test/src/admin/methods.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

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

Deno.test("runtime owns its production stream configs across restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const nats = await connect({
      servers: runtime.natsUrl,
      authenticator: credsAuthenticator(
        await Deno.readFile(
          join(runtime.workdir, "nats/creds/trellis-auth.creds"),
        ),
      ),
    });
    try {
      const manager = await jetstreamManager(nats);
      const configs = async () => {
        const streams = await manager.streams.list().next();
        return Object.fromEntries(
          streams.filter(({ config }) =>
            ["trellis", "JOBS", "JOBS_WORK", "JOBS_ADVISORIES"].includes(
              config.name,
            )
          ).map(({ config }) => [config.name, {
            subjects: config.subjects,
            retention: config.retention,
            storage: config.storage,
            discard: config.discard,
            max_age: config.max_age,
            max_msgs: config.max_msgs,
            max_msgs_per_subject: config.max_msgs_per_subject,
            max_bytes: config.max_bytes,
            allow_direct: config.allow_direct,
            sources: config.sources,
          }]),
        );
      };
      const expected: Awaited<ReturnType<typeof configs>> = {
        trellis: {
          subjects: ["events.>"],
          retention: "limits",
          storage: "file",
          discard: "old",
          max_age: 604_800_000_000_000,
          max_msgs: -1,
          max_msgs_per_subject: -1,
          max_bytes: -1,
          allow_direct: false,
          sources: undefined,
        },
        JOBS: {
          subjects: ["trellis.jobs.>"],
          retention: "limits",
          storage: "file",
          discard: "old",
          max_age: 0,
          max_msgs: -1,
          max_msgs_per_subject: -1,
          max_bytes: -1,
          allow_direct: true,
          sources: undefined,
        },
        JOBS_WORK: {
          subjects: ["trellis.work.>"],
          retention: "workqueue",
          storage: "file",
          discard: "old",
          max_age: 0,
          max_msgs: -1,
          max_msgs_per_subject: -1,
          max_bytes: -1,
          allow_direct: true,
          sources: [{
            name: "JOBS",
            subject_transforms: [
              { src: "trellis.jobs.*.*.*.created", dest: "trellis.work.$1.$2" },
              { src: "trellis.jobs.*.*.*.retried", dest: "trellis.work.$1.$2" },
            ],
          }],
        },
        JOBS_ADVISORIES: {
          subjects: ["$JS.EVENT.ADVISORY.CONSUMER.MAX_DELIVERIES.>"],
          retention: "limits",
          storage: "file",
          discard: "new",
          max_age: 0,
          max_msgs: -1,
          max_msgs_per_subject: -1,
          max_bytes: -1,
          allow_direct: false,
          sources: undefined,
        },
      };
      assertEquals(await configs(), expected);
      await runtime.restartControlPlane();
      assertEquals(await configs(), expected);
    } finally {
      await nats.close();
    }
  });
});

Deno.test("generated TypeScript caller reaches Rust provider", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust",
      contract: participants.OperationProvider.participant,
    });
    const startProvider = () =>
      new Deno.Command("setsid", {
        args: [
          "cargo",
          "run",
          "--config",
          `patch.crates-io.trellis-rs.path=${
            JSON.stringify(
              fromFileUrl(
                new URL("../../crates/trellis", import.meta.url),
              ),
            )
          }`,
          "--bin",
          "trellis-runtime-acceptance",
          "--manifest-path",
          fromFileUrl(
            new URL(
              "../../integration/fixtures/runtime/Cargo.toml",
              import.meta.url,
            ),
          ),
        ],
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          TRELLIS_IDENTITY_SEED: identity.seed,
          CARGO_TARGET_DIR: fromFileUrl(
            new URL("../../target", import.meta.url),
          ),
        },
        stdout: "inherit",
        stderr: "inherit",
      }).spawn();
    let process = startProvider();
    let exited = false;
    let status = process.status.then((status) => {
      exited = true;
      return status;
    });
    try {
      const client = await runtime.connectClient({
        name: "cross-language",
        contract: participants.Caller.participant,
      });
      const response = await runtime.waitFor(async () => {
        if (exited) {
          throw new Error(
            `Rust provider exited: ${JSON.stringify(await status)}`,
          );
        }
        const result = await client.echo({ value: "from TypeScript" }, {
          timeout: 1000,
        });
        return result.isOk() ? result.orThrow() : false;
      }, { timeoutMs: 120_000 });
      assertEquals(response.value, "Rust received from TypeScript");

      const firstFeedAbort = new AbortController();
      const secondFeedAbort = new AbortController();
      const firstFeed = await client.watch({}, {
        signal: firstFeedAbort.signal,
      })
        .orThrow();
      const secondFeed = await client.watch({}, {
        signal: secondFeedAbort.signal,
      })
        .orThrow();
      const firstFeedIterator = firstFeed[Symbol.asyncIterator]();
      const secondFeedIterator = secondFeed[Symbol.asyncIterator]();
      assert(
        (await firstFeedIterator.next()).value?.value.startsWith("rust-feed-"),
      );
      assert(
        (await secondFeedIterator.next()).value?.value.startsWith("rust-feed-"),
      );
      firstFeedAbort.abort();
      assert(
        (await secondFeedIterator.next()).value?.value.startsWith("rust-feed-"),
      );
      secondFeedAbort.abort();

      const failed = await client.work({ value: "failure" }).start().orThrow();
      assertEquals((await failed.wait().orThrow()).state, "failed");

      const live = await client.work({ value: "reconnect-live" }).start()
        .orThrow();
      await runtime.waitFor(async () =>
        (await live.get().orThrow()).progress?.value === "persisted"
      );

      // Repeating the exact invocation must return the persisted typed snapshot
      // through the accepted path without executing the operation again.
      let replayedAccepted: { snapshot: { progress?: unknown } } | undefined;
      const replayed = await client.work({ value: "reconnect-live" }).start(
        {
          onAccepted: (event: { snapshot: { progress?: unknown } }) => {
            replayedAccepted = event;
          },
        },
        { invocationId: live.id },
      ).orThrow();
      assertEquals(replayed.id, live.id);
      assertEquals((await replayed.get().orThrow()).state, "running");
      assertEquals(
        replayedAccepted?.snapshot.progress,
        persistedProgress,
      );

      await client.connection.close();
      assertEquals(client.connection.status.phase, "closed");

      const reconnected = await runtime.connectClient({
        name: "reconnected-caller",
        contract: participants.Caller.participant,
      });
      const resumedLive = reconnected.work.resume(live);
      assertEquals(
        (await resumedLive.get().orThrow()).progress,
        persistedProgress,
      );
      const watched = (await resumedLive.watch({ updates: true }).orThrow())
        [Symbol.asyncIterator]();
      const initial = (await watched.next()).value;
      assert(initial?.type !== "update");
      assertEquals(initial?.snapshot.progress, persistedProgress);

      await runtime.deployments.create({
        id: "operation-intruder",
        kind: "service",
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
        await assertRejects(async () => {
          const deniedWatch = await intruderOperation.watch().orThrow();
          await deniedWatch[Symbol.asyncIterator]().next();
        });
        assert((await intruderOperation.signal("Continue", {
          value: "intruder",
        })).isErr());
        assert((await intruderOperation.cancel()).isErr());

        const signal = await resumedLive.signal("Continue", {
          value: "emit update",
        }).orThrow();
        let update = (await watched.next()).value;
        while (update?.type !== "update") update = (await watched.next()).value;
        assert(update);
        assertEquals(update.update.value, "transient");
        assertEquals(update.update, transientProgress);
        const afterUpdate = await resumedLive.get().orThrow();
        assert(afterUpdate.revision >= signal.snapshot.revision);
        assertEquals(afterUpdate.progress, signal.snapshot.progress);

        const late = (await resumedLive.watch({ updates: true }).orThrow())
          [Symbol.asyncIterator]();
        const lateInitial = (await late.next()).value;
        assert(lateInitial?.type !== "update");
        assertEquals(lateInitial?.snapshot.progress, persistedProgress);
        await resumedLive.cancel().orThrow();
        let lateTerminal = (await late.next()).value;
        while (lateTerminal?.snapshot.state !== "cancelled") {
          assert(lateTerminal?.type !== "update");
          lateTerminal = (await late.next()).value;
        }
        assertEquals(lateTerminal?.snapshot.state, "cancelled");
        let watchedTerminal = (await watched.next()).value;
        while (watchedTerminal?.snapshot.state !== "cancelled") {
          watchedTerminal = (await watched.next()).value;
        }
        assertEquals(watchedTerminal.snapshot.state, "cancelled");
      } finally {
        await intruder.stop();
        assertEquals(await intruderExit, undefined);
      }

      const resumable = await reconnected.work({ value: "resume" }).start()
        .orThrow();
      await runtime.waitFor(async () =>
        (await resumable.get().orThrow()).progress?.value === "persisted"
      );
      const bytes = Uint8Array.from(
        { length: 131_073 },
        (_, index) => index % 251,
      );
      const transferred = await reconnected.upload({ value: "upload" })
        .transfer(
          bytes,
        ).start().orThrow();
      await runtime.waitFor(async () =>
        (await transferred.operation.get().orThrow()).transfer
          ?.transferredBytes === bytes.length
      );
      Deno.kill(-process.pid, "SIGKILL");
      await status;
      exited = false;
      process = startProvider();
      status = process.status.then((value) => {
        exited = true;
        return value;
      });
      await runtime.waitFor(
        async () =>
          (await reconnected.echo({ value: "from TypeScript" }, {
            timeout: 1000,
          }))
            .isOk(),
        { timeoutMs: 120_000 },
      );
      const resumed = await runtime.waitFor(async () => {
        const snapshot = await resumable.get().orThrow();
        return snapshot.state === "completed" ? snapshot : false;
      }, { timeoutMs: 120_000 });
      assertEquals(resumed.state, "completed");
      assertEquals(resumed.output?.value, "resumed");

      const uploaded = await runtime.waitFor(async () => {
        const snapshot = await transferred.operation.get().orThrow();
        return snapshot.state === "completed" ? snapshot : false;
      }, { timeoutMs: 120_000 });
      assertEquals(uploaded.state, "completed");
      assertEquals(uploaded.output?.value, `upload:${bytes.length}:true`);

      // Generated Rust caller leg: the same running Rust provider serves the
      // generated Rust Feed and Operation calls.
      const callerChild = new Deno.Command("cargo", {
        args: [
          "run",
          "--config",
          `patch.crates-io.trellis-rs.path=${
            JSON.stringify(
              fromFileUrl(
                new URL("../../crates/trellis", import.meta.url),
              ),
            )
          }`,
          "--bin",
          "caller",
          "--manifest-path",
          fromFileUrl(
            new URL(
              "../../integration/fixtures/runtime/Cargo.toml",
              import.meta.url,
            ),
          ),
        ],
        env: {
          TRELLIS_URL: runtime.trellisUrl,
          XDG_CONFIG_HOME: join(runtime.workdir, "rust-caller-config"),
          CARGO_TARGET_DIR: fromFileUrl(
            new URL("../../target", import.meta.url),
          ),
        },
        stdout: "piped",
        stderr: "inherit",
      }).spawn();
      const callerReader = callerChild.stdout.pipeThrough(
        new TextDecoderStream(),
      ).getReader();
      let callerOutput = "";
      while (!callerOutput.includes("rust login ")) {
        const chunk = await callerReader.read();
        assert(!chunk.done, callerOutput);
        callerOutput += chunk.value;
      }
      const callerLoginUrl = callerOutput.match(/rust login (\S+)/)?.[1];
      assert(callerLoginUrl);
      await runtime.completeClientAuth({
        loginUrl: callerLoginUrl,
        sessionKey: "completed-by-rust",
        mode: "session_key",
      });
      while (!callerOutput.includes("rust caller complete")) {
        const chunk = await callerReader.read();
        assert(!chunk.done, callerOutput);
        callerOutput += chunk.value;
      }
      assert((await callerChild.status).success, callerOutput);
    } finally {
      if (!exited) Deno.kill(-process.pid, "SIGTERM");
      await status;
    }
  });
});

Deno.test("simultaneous Feed controls remain creator and feed scoped", async () => {
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
      contract: participants.Caller.participant,
    });
    const firstAbort = new AbortController();
    const secondAbort = new AbortController();
    try {
      const first = await caller.watch({}, { signal: firstAbort.signal })
        .orThrow();
      const second = await caller.watch({}, { signal: secondAbort.signal })
        .orThrow();
      const firstIterator = first[Symbol.asyncIterator]();
      const secondIterator = second[Symbol.asyncIterator]();
      assert((await firstIterator.next()).value?.value.startsWith("feed-1-"));
      assert((await secondIterator.next()).value?.value.startsWith("feed-2-"));

      firstAbort.abort();
      await runtime.waitFor(() => cancelled === 1);
      assert((await secondIterator.next()).value?.value.startsWith("feed-2-"));
      assertEquals(cancelled, 1);
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

Deno.test("generated Rust resources use live NATS", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "rust-resources",
      contract: participants.Provider.participant,
    });
    const child = new Deno.Command("cargo", {
      args: [
        "run",
        "--config",
        `patch.crates-io.trellis-rs.path=${
          JSON.stringify(
            fromFileUrl(new URL("../../crates/trellis", import.meta.url)),
          )
        }`,
        "--bin",
        "resources",
        "--manifest-path",
        fromFileUrl(
          new URL(
            "../../integration/fixtures/runtime/Cargo.toml",
            import.meta.url,
          ),
        ),
      ],
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_IDENTITY_SEED: identity.seed,
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdin: "piped",
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const reader = child.stdout.pipeThrough(new TextDecoderStream())
      .getReader();
    let output = "";
    while (!output.includes("rust resources ready")) {
      const chunk = await reader.read();
      assert(!chunk.done, output);
      output += chunk.value;
    }
    const stdin = child.stdin.getWriter();
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
          )
        ),
      ]);
      assert(!chunk.done, output);
      output += chunk.value;
    }
    if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
      console.error(runtime.controlPlaneOutput());
    }
    const status = await child.status;
    assert(status.success);

    await runtime.contracts.install({
      contract: participants.StateCaller.participant,
    });
    const stateChild = new Deno.Command("cargo", {
      args: [
        "run",
        "--config",
        `patch.crates-io.trellis-rs.path=${
          JSON.stringify(
            fromFileUrl(new URL("../../crates/trellis", import.meta.url)),
          )
        }`,
        "--bin",
        "resources",
        "--manifest-path",
        fromFileUrl(
          new URL(
            "../../integration/fixtures/runtime/Cargo.toml",
            import.meta.url,
          ),
        ),
      ],
      env: {
        TRELLIS_URL: runtime.trellisUrl,
        TRELLIS_STATE_ACCEPTANCE: "1",
        XDG_CONFIG_HOME: join(runtime.workdir, "rust-state-config"),
        CARGO_TARGET_DIR: fromFileUrl(
          new URL("../../target", import.meta.url),
        ),
      },
      stdout: "piped",
      stderr: "inherit",
    }).spawn();
    const stateReader = stateChild.stdout.pipeThrough(new TextDecoderStream())
      .getReader();
    let stateOutput = "";
    while (!stateOutput.includes("rust login ")) {
      const chunk = await stateReader.read();
      assert(!chunk.done, stateOutput);
      stateOutput += chunk.value;
    }
    const loginUrl = stateOutput.match(/rust login (\S+)/)?.[1];
    assert(loginUrl);
    await runtime.completeClientAuth({
      loginUrl,
      sessionKey: "completed-by-rust",
      mode: "session_key",
    });
    while (!stateOutput.includes("rust state complete")) {
      const chunk = await stateReader.read();
      assert(!chunk.done, stateOutput);
      stateOutput += chunk.value;
    }
    assert((await stateChild.status).success);
  });
});

Deno.test("generated runtime workflows", async (t) => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "provider",
      contract: participants.Provider.participant,
    });
    const wrongSeed = `${identity.seed.slice(0, 5)}${
      identity.seed[5] === "A" ? "B" : "A"
    }${identity.seed.slice(6)}`;
    assert((await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.Provider.participant,
      seed: wrongSeed,
    })).isErr());
    assert((await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant: participants.AdminService.participant,
      seed: identity.seed,
    })).isErr());
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
    let serviceExit: Promise<unknown> | undefined;
    try {
      await service.handleEcho(({ input }) => Result.ok(input));
      await service.handleWork(async ({ input, op, signal }) => {
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
              signal.addEventListener("abort", () => resolve(), { once: true });
            });
          }
          cancellationObserved.resolve();
          lateCompletionRejected.resolve(
            (await op.complete({ value: "too late" })).isErr(),
          );
          return op.defer();
        }
        return await op.complete({ value: `completed ${input.value}` })
          .orThrow();
      });
      let attempts = 0;
      service.jobs.work.handle(({ job }) =>
        Promise.resolve(
          ++attempts === 1
            ? Result.err(new RetryJobError())
            : Result.ok(job.payload),
        )
      );
      let releaseFirstKeyed!: () => void;
      const firstKeyedRelease = new Promise<void>((resolve) => {
        releaseFirstKeyed = resolve;
      });
      const keyedRuns = new Map<string, number>();
      service.jobs.keyedWork.handle(async ({ job }) => {
        keyedRuns.set(
          job.payload.value,
          (keyedRuns.get(job.payload.value) ?? 0) + 1,
        );
        if (job.payload.value === "first") await firstKeyedRelease;
        return Result.ok(job.payload);
      }, { concurrency: 2 });
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
        await transfer.completed().orThrow();
        return await op.complete(input).orThrow();
      });
      let received: string | undefined;
      let coverageRecoveryEffects = 0;
      let coldRevokedEffects = 0;
      await service.onChanged(({ event }) => {
        if (event.value === "coverage-recovery") coverageRecoveryEffects += 1;
        if (event.value === "cold-revoked") coldRevokedEffects += 1;
        received = event.value;
        return Result.ok(undefined);
      }).orThrow();
      serviceExit = service.wait().catch((error: unknown) => error);
      const client = await runtime.connectClient({
        name: "caller",
        contract: participants.Caller.participant,
      });
      await t.step("operation executes and completes", async () => {
        const operation = await client.work({ value: "work" }).start()
          .orThrow();
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.state, "completed");
        assertEquals(terminal.output?.value, "completed work");
      });
      await t.step(
        "cancellation aborts and fences the active handler",
        async () => {
          const operation = await client.work({ value: "cancel-active" })
            .start()
            .orThrow();
          await runtime.waitFor(async () =>
            (await operation.get().orThrow()).state === "running"
          );
          assertEquals((await operation.cancel().orThrow()).state, "cancelled");
          await cancellationObserved.promise;
          assert(await lateCompletionRejected.promise);
          assertEquals((await operation.get().orThrow()).state, "cancelled");
        },
      );
      await t.step(
        "same principal controls operation after reconnect",
        async () => {
          const cleanupConsole = await runtime.connectClient({
            name: "operation-reconnect-console",
            contract: webParticipants.Console.participant,
          });
          const existingSessions = new Set(
            (await cleanupConsole.sessionsList({
              participantId: participants.Caller.participant.id,
              state: "active",
            }).orThrow()).items.map((session) => session.sessionId),
          );
          const existingAdminSessions = new Set(
            (await cleanupConsole.sessionsList({
              participantId: adminParticipant.id,
              state: "active",
            }).orThrow()).items.map((session) => session.sessionId),
          );
          const cleanupAdmin = await runtime.connectClient({
            name: "operation-reconnect-cleanup",
            contract: adminParticipant,
          });
          try {
            const origin = await runtime.connectClient({
              name: "operation-reconnect-origin",
              contract: participants.Caller.participant,
            });
            const operation = await origin.work({ value: "reconnect-live" })
              .start()
              .orThrow();
            await runtime.waitFor(async () =>
              (await operation.get().orThrow()).state === "running"
            );
            await origin.connection.close();

            const reconnected = await runtime.connectClient({
              name: "operation-reconnect-current",
              contract: participants.Caller.participant,
            });
            try {
              const resumed = reconnected.work.resume(operation);
              assertEquals((await resumed.get().orThrow()).state, "running");
              const sibling = await runtime.services.createInstance({
                name: "provider-sibling",
                contract: participants.Provider.participant,
              });
              assertEquals(sibling.deploymentId, identity.deploymentId);
              const siblingService = await TrellisService.connect({
                trellisUrl: runtime.trellisUrl,
                participant: participants.Provider.participant,
                name: "provider-sibling",
                seed: sibling.seed,
              }).orThrow();
              const siblingExit = siblingService.wait().catch((error) => error);
              siblingService.handleWork(({ op }) =>
                Promise.resolve(op.defer())
              );
              try {
                if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                  console.error("TRELLIS_TRACE replica_watches_begin");
                }
                const watches = await Promise.all([
                  resumed.watch({ updates: true }).orThrow(),
                  resumed.watch({ updates: true }).orThrow(),
                ]).then((streams) =>
                  streams.map((stream) => stream[Symbol.asyncIterator]())
                );
                for (const watch of watches) {
                  assertEquals(
                    (await watch.next()).value?.snapshot.state,
                    "running",
                  );
                }
                if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                  console.error(
                    "TRELLIS_TRACE replica_initial_snapshots_received",
                  );
                }
                const nats = await connect({
                  servers: runtime.natsUrl,
                  authenticator: credsAuthenticator(
                    await Deno.readFile(
                      join(runtime.workdir, "nats/creds/trellis-auth.creds"),
                    ),
                  ),
                });
                try {
                  const encode = (value: string) =>
                    new TextEncoder().encode(value).toBase64({
                      alphabet: "base64url",
                      omitPadding: true,
                    });
                  const subject = `operation.v1.${
                    encode("runtime-trellis.runtime@v1")
                  }.${
                    encode(identity.deploymentId)
                  }.Work.updates.${operation.id}`;
                  const forged = natsHeaders();
                  forged.set("trellis-owner-executor", identity.instanceId);
                  forged.set("trellis-owner-epoch", "1");
                  forged.set("trellis-update-sequence", "1");
                  forged.set("trellis-update-time", new Date().toISOString());
                  nats.publish(subject, JSON.stringify({ value: "forged" }), {
                    headers: forged,
                    reply: `${subject}.owner.${identity.instanceId}`,
                  });
                  await nats.flush();
                  if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                    console.error("TRELLIS_TRACE forged_update_flushed");
                  }
                } finally {
                  await nats.close();
                }
                if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                  console.error("TRELLIS_TRACE continue_signal_begin");
                }
                await resumed.signal("Continue", { value: "continue" })
                  .orThrow();
                if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                  console.error("TRELLIS_TRACE continue_signal_complete");
                }
                for (const [index, watch] of watches.entries()) {
                  let update = (await watch.next()).value;
                  while (update && update.type !== "update") {
                    if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                      console.error(
                        `TRELLIS_TRACE replica_watch_frame index=${index} type=${update.type}`,
                      );
                    }
                    update = (await watch.next()).value;
                  }
                  if (Deno.env.get("TRELLIS_TRACE_AUTH") === "1") {
                    console.error(
                      `TRELLIS_TRACE replica_watch_update index=${index} present=${
                        Boolean(update)
                      }`,
                    );
                  }
                  assert(update);
                  assertEquals(update.update, transientProgress);
                }
              } finally {
                await siblingService.stop();
                assertEquals(await siblingExit, undefined);
              }
              const late = (await resumed.watch({ updates: true }).orThrow())
                [Symbol.asyncIterator]();
              assert((await late.next()).value?.type !== "update");
              const cancel = await resumed.cancel();
              if (cancel.isErr()) {
                throw new Error(
                  `reconnected cancel failed: ${JSON.stringify(cancel.error)}`,
                );
              }
              assertEquals(cancel.orThrow().state, "cancelled");
            } finally {
              await reconnected.connection.close();
            }
          } finally {
            try {
              const sessions = await cleanupConsole.sessionsList({
                participantId: participants.Caller.participant.id,
                state: "active",
              }).orThrow();
              for (const session of sessions.items) {
                if (!existingSessions.has(session.sessionId)) {
                  await cleanupAdmin.sessionsRevoke({
                    sessionId: session.sessionId,
                    expectedVersion: session.version,
                    idempotencyKey: crypto.randomUUID(),
                    reason: "operation reconnect cleanup",
                  }).orThrow();
                }
              }
            } finally {
              await cleanupAdmin.connection.close();
              try {
                const sessions = await cleanupConsole.sessionsList({
                  participantId: adminParticipant.id,
                  state: "active",
                }).orThrow();
                for (const session of sessions.items) {
                  if (!existingAdminSessions.has(session.sessionId)) {
                    await cleanupConsole.sessionsRevoke({
                      sessionId: session.sessionId,
                      expectedVersion: session.version,
                      idempotencyKey: crypto.randomUUID(),
                      reason: "operation reconnect cleanup",
                    }).orThrow();
                  }
                }
              } finally {
                await cleanupConsole.sessionsLogout({});
              }
            }
          }
        },
      );
      await t.step("event reaches generated subscriber", async () => {
        await service.publishChanged({ value: "published" }).orThrow();
        assertEquals(await runtime.waitFor(() => received), "published");
      });
      await t.step(
        "durable event waits for exact revocation-watch coverage",
        async () => {
          const nats = await connect({
            servers: runtime.natsUrl,
            authenticator: credsAuthenticator(
              await Deno.readFile(
                join(runtime.workdir, "nats/creds/trellis-auth.creds"),
              ),
            ),
          });
          const natsExit = nats.closed();
          try {
            const manager = await jetstreamManager(nats);
            const streams = await manager.streams.list().next();
            const eventStream = streams.find((stream) =>
              stream.config.subjects?.some((subject) =>
                subject.startsWith("events.")
              )
            );
            if (!eventStream) throw new Error("event stream missing");
            const eventConsumer = (await manager.consumers.list(
              eventStream.config.name,
            ).next()).find((consumer) =>
              consumer.config.filter_subject ===
                "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed" ||
              consumer.config.filter_subjects?.includes(
                "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed",
              )
            );
            if (!eventConsumer) throw new Error("event consumer missing");
            const retainedEvent = await manager.streams.getMessage(
              eventStream.config.name,
              {
                last_by_subj:
                  "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed",
              },
            );
            if (!retainedEvent) throw new Error("retained event missing");
            const eventContextDigest = retainedEvent.header?.get(
              "authorization-context",
            );
            if (!eventContextDigest) {
              throw new Error("retained event context digest missing");
            }
            received = undefined;
            let contextStream: string | undefined;
            let contextSubjectPrefix: string | undefined;
            for (const stream of await manager.streams.list().next()) {
              for (const subject of stream.config.subjects ?? []) {
                if (!subject.startsWith("$KV.") || !subject.endsWith(">")) {
                  continue;
                }
                const prefix = subject.slice(0, -1);
                try {
                  const stored = await manager.streams.getMessage(
                    stream.config.name,
                    { last_by_subj: `${prefix}${eventContextDigest}` },
                  );
                  if (!stored) continue;
                  contextStream = stream.config.name;
                  contextSubjectPrefix = prefix;
                  break;
                } catch {
                  // This KV stream does not own authorization contexts.
                }
              }
              if (contextStream) break;
            }
            if (!contextStream || !contextSubjectPrefix) {
              throw new Error("authorization context stream missing");
            }
            const revokedIdentity = await runtime.services.createInstance({
              name: "cold-revoked-publisher",
              contract: participants.Provider.participant,
            });
            const revokedPublisher = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.Provider.participant,
              seed: revokedIdentity.seed,
            }).orThrow();
            const revokedPublisherExit = revokedPublisher.wait().catch((
              error,
            ) => error);
            await manager.consumers.pause(
              eventStream.config.name,
              eventConsumer.name,
              new Date(Date.now() + 60_000),
            );
            await revokedPublisher.publishChanged({ value: "cold-revoked" })
              .orThrow();
            const revokedEvent = await manager.streams.getMessage(
              eventStream.config.name,
              {
                last_by_subj:
                  "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed",
              },
            );
            const revokedDigest = revokedEvent?.header?.get(
              "authorization-context",
            );
            if (!revokedEvent || !revokedDigest) {
              throw new Error("cold revoked event context missing");
            }
            await revokedPublisher.stop();
            assertEquals(await revokedPublisherExit, undefined);
            await runtime.services.disableInstance({
              instanceId: revokedIdentity.instanceId,
              expectedVersion: 1n,
              idempotencyKey: crypto.randomUUID(),
              reason: "cold revocation acceptance",
            });
            await runtime.waitFor(async () =>
              Boolean(
                await manager.streams.getMessage(contextStream, {
                  last_by_subj:
                    `${contextSubjectPrefix}revocation.${revokedDigest}`,
                }),
              )
            );
            await manager.consumers.resume(
              eventStream.config.name,
              eventConsumer.name,
            );
            await runtime.waitFor(async () =>
              (await manager.consumers.info(
                eventStream.config.name,
                eventConsumer.name,
              )).ack_floor.stream_seq >= revokedEvent.seq
            );
            assertEquals(coldRevokedEffects, 0);
            const coldIdentity = await runtime.services.createInstance({
              name: "cold-event-publisher",
              contract: participants.Provider.participant,
            });
            const coldPublisher = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.Provider.participant,
              name: "cold-event-publisher",
              seed: coldIdentity.seed,
            }).orThrow();
            const coldPublisherExit = coldPublisher.wait().catch((error) =>
              error
            );
            let coldEventContextDigest: string | undefined;
            try {
              await manager.consumers.pause(
                eventStream.config.name,
                eventConsumer.name,
                new Date(Date.now() + 60_000),
              );
              await coldPublisher.publishChanged({ value: "coverage-recovery" })
                .orThrow();
              const coldEvent = await manager.streams.getMessage(
                eventStream.config.name,
                {
                  last_by_subj:
                    "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed",
                },
              );
              assert(coldEvent);
              coldEventContextDigest = coldEvent.header?.get(
                "authorization-context",
              );
              assert(coldEventContextDigest);
              assert(coldEventContextDigest !== eventContextDigest);
              await manager.streams.delete(contextStream);
              await manager.consumers.resume(
                eventStream.config.name,
                eventConsumer.name,
              );
              await runtime.waitFor(async () => {
                const consumers = await manager.consumers.list(
                  eventStream.config.name,
                ).next();
                return consumers.some((consumer) =>
                  (consumer.config.filter_subjects?.includes(
                    "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed",
                  ) ?? consumer.config.filter_subject ===
                      "events.v1.cnVudGltZS10cmVsbGlzLnJ1bnRpbWVAdjE.Changed") &&
                  consumer.delivered.consumer_seq >
                    consumer.ack_floor.consumer_seq
                );
              }, { timeoutMs: 15_000 });
              assertEquals(coverageRecoveryEffects, 0);
            } finally {
              await coldPublisher.stop();
              const error = await coldPublisherExit;
              assert(
                !(error instanceof Error),
                error instanceof Error ? error.message : undefined,
              );
              await manager.consumers.resume(
                eventStream.config.name,
                eventConsumer.name,
              ).catch(() => undefined);
              await runtime.restartControlPlane();
              assert(coldEventContextDigest);
              await runtime.waitFor(async () => {
                try {
                  return Boolean(
                    await manager.streams.getMessage(
                      contextStream,
                      {
                        last_by_subj: contextSubjectPrefix +
                          coldEventContextDigest,
                      },
                    ),
                  );
                } catch {
                  return false;
                }
              });
              await manager.consumers.resume(
                eventStream.config.name,
                eventConsumer.name,
              );
            }
            assertEquals(
              await runtime.waitFor(
                () =>
                  received === "coverage-recovery" && coverageRecoveryEffects,
                { timeoutMs: 30_000 },
              ),
              1,
            );
          } finally {
            await nats.close();
            const error = await natsExit;
            if (error) {
              throw new Error(
                "revocation coverage NATS connection closed unexpectedly",
                { cause: error },
              );
            }
          }
        },
      );
      await t.step("job retries then completes", async () => {
        const startedAt = Date.now();
        const job = await service.jobs.work.create({ value: "retried" })
          .orThrow();
        const initial = await job.get().orThrow();
        assert(initial.deadline);
        assertEquals(
          Date.parse(initial.deadline) - Date.parse(initial.createdAt),
          5_000,
        );
        const terminal = await job.wait().orThrow();
        assertEquals(terminal.state, "completed");
        assertEquals(terminal.result, { value: "retried" });
        assertEquals(attempts, 2);
        assert(Date.now() - startedAt >= 200);
      });
      await t.step("keyed wait does not consume deliveries", async () => {
        const first = await service.jobs.keyedWork.create({
          key: "shared",
          value: "first",
        }).orThrow();
        await runtime.waitFor(async () =>
          (await first.get().orThrow()).state === "active"
        );
        const second = await service.jobs.keyedWork.create({
          key: "shared",
          value: "second",
        }).orThrow();
        const horizon = Date.now() + 26_000;
        await runtime.waitFor(async () => {
          const snapshot = await second.get().orThrow();
          assertEquals(snapshot.tries, 0);
          assertEquals(snapshot.state, "pending");
          return Date.now() >= horizon;
        }, { timeoutMs: 30_000 });
        assertEquals(keyedRuns.get("second"), undefined);

        releaseFirstKeyed();
        assertEquals((await first.wait().orThrow()).state, "completed");
        assertEquals((await second.wait().orThrow()).state, "completed");
        assertEquals(keyedRuns, new Map([["first", 1], ["second", 1]]));
      });
      await t.step(
        "Jobs Summary matches filtered Query across pages",
        async () => {
          const console = await runtime.connectClient({
            name: "jobs-summary-console",
            contract: webParticipants.Console.participant,
          });
          try {
            for (let index = 0; index < 3; index += 1) {
              const job = await service.jobs.work.create({
                value: `summary-parity-${index}`,
              }).orThrow();
              assertEquals((await job.wait().orThrow()).state, "completed");
            }
            const filter = {
              state: ["completed" as const],
              groupBy: "state" as const,
            };
            const first = await runtime.waitFor(async () => {
              const page = await console.jobsQuery({
                ...filter,
                page: { limit: 1 },
              }).orThrow();
              return page.items.length === 1 && page.page.nextCursor
                ? page
                : false;
            });
            const second = await console.jobsQuery({
              ...filter,
              page: { limit: 1, cursor: first.page.nextCursor },
            }).orThrow();
            const summary = await console.jobsSummary(filter).orThrow();

            assertEquals(first.items.length, 1);
            assertEquals(second.items.length, 1);
            assertEquals(summary.count, 6n);
            assertEquals(summary.stats.total, 6n);
            assertEquals(summary.stats.byState, { completed: 6n });
            assertEquals(
              summary.groups.map(({ state, count }) => ({ state, count })),
              [{
                state: "completed",
                count: 6n,
              }],
            );
          } finally {
            await console.connection.close();
          }
        },
      );
      await t.step("transfer preserves exact bytes", async () => {
        const bytes = Uint8Array.from(
          { length: 131073 },
          (_, index) => index % 251,
        );
        const operation = await client.upload({ value: "bytes" }).transfer(
          bytes,
        ).start().orThrow();
        const terminal = await operation.wait().orThrow();
        assertEquals(terminal.terminal.state, "completed");
        assertEquals(terminal.transferred.size, bytes.length);
        const entry = await service.store.files.waitFor(operation.operation.id)
          .orThrow();
        assertEquals(await entry.bytes().orThrow(), bytes);
      });
      await t.step(
        "feed controls are isolated per simultaneous feed",
        async () => {
          const firstAbort = new AbortController();
          const secondAbort = new AbortController();
          try {
            const first = await client.watch({}, {
              signal: AbortSignal.any([
                firstAbort.signal,
                AbortSignal.timeout(10_000),
              ]),
            }).orThrow();
            const second = await client.watch({}, {
              signal: AbortSignal.any([
                secondAbort.signal,
                AbortSignal.timeout(10_000),
              ]),
            }).orThrow();
            const firstIterator = first[Symbol.asyncIterator]();
            const secondIterator = second[Symbol.asyncIterator]();
            assert(
              (await firstIterator.next()).value?.value.startsWith("feed-1-"),
            );
            assert(
              (await secondIterator.next()).value?.value.startsWith("feed-2-"),
            );

            firstAbort.abort();
            await runtime.waitFor(() => cancelledFeeds === 1);
            assert(
              (await secondIterator.next()).value?.value.startsWith("feed-2-"),
            );
            assertEquals(cancelledFeeds, 1);
          } finally {
            firstAbort.abort();
            secondAbort.abort();
          }
          await runtime.waitFor(() => cancelledFeeds === 2);
        },
      );
      await t.step("state survives control-plane restart", async () => {
        await client.state.saved.set({ value: "durable" }).orThrow();
        await runtime.restartControlPlane();
        const stored = await client.state.saved.get().orThrow();
        assert(stored);
        assertEquals(stored.value.value, "durable");
      });
      await t.step(
        "authority denial, success, and session revocation",
        async () => {
          await runtime.connectClient({
            name: "denied",
            contract: participants.Denied.participant,
          });
          assertEquals(
            (await client.echo({ value: "allowed" }).orThrow()).value,
            "allowed",
          );
          const admin = await runtime.connectClient({
            name: "admin",
            contract: adminParticipant,
          });
          const consoleClient = await runtime.connectClient({
            name: "console",
            contract: webParticipants.Console.participant,
          });
          await runtime.deployments.create({
            id: "native-admin",
            kind: "service",
          });
          const nativeKey = await runtime.registerService({
            name: "native-admin",
            contract: participants.AdminService.participant,
            deployment: "native-admin",
          });
          const nativeSiblingKey = await runtime.services.createInstance({
            name: "native-admin-sibling",
            contract: participants.AdminService.participant,
            deployment: "native-admin",
          });
          let nativeAdmin = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.AdminService.participant,
            name: "native-admin",
            seed: nativeKey.seed,
          }).orThrow();
          let nativeAdminExit = nativeAdmin.wait().catch((error: unknown) =>
            error
          );
          const deniedOwnDeployment = await nativeAdmin.deploymentsGet({
            deploymentId: nativeKey.deploymentId,
          });
          assert(
            deniedOwnDeployment.isErr(),
            "own deployment read requires administrator privilege",
          );
          assertEquals(deniedOwnDeployment.error.name, "AuthError");
          const deniedBinding = (await admin.grantsList({
            participantId: participants.Denied.participant.id,
            state: "active",
          }).orThrow()).items[0];
          assert(deniedBinding);
          const deniedMutation = await nativeAdmin.grantsRevoke({
            ownerKind: deniedBinding.ownerKind,
            ownerId: deniedBinding.ownerId,
            participantId: deniedBinding.participantId,
            expectedRevision: deniedBinding.revision,
            idempotencyKey: crypto.randomUUID(),
            reason: "native administrator acceptance",
          });
          assert(deniedMutation.isErr());

          const nativeBinding = (await admin.grantsGet({
            ownerKind: "deployment",
            ownerId: nativeKey.deploymentId,
            participantId: participants.AdminService.participant.id,
          }).orThrow()).binding;
          assert(nativeBinding);
          await admin.grantsSet({
            ownerKind: nativeBinding.ownerKind,
            ownerId: nativeBinding.ownerId,
            participantId: nativeBinding.participantId,
            installedRevision: nativeBinding.installedRevision,
            grants: nativeBinding.grants,
            platformPrivileges: ["trellis.auth::admin"],
            expiresAt: nativeBinding.expiresAt,
            expectedRevision: nativeBinding.revision,
            idempotencyKey: crypto.randomUUID(),
          }).orThrow();
          await nativeAdmin.stop();
          const firstNativeExit = await nativeAdminExit;
          assert(
            !(firstNativeExit instanceof Error),
            firstNativeExit instanceof Error
              ? firstNativeExit.message
              : undefined,
          );
          nativeAdmin = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.AdminService.participant,
            name: "native-admin",
            seed: nativeKey.seed,
          }).orThrow();
          nativeAdminExit = nativeAdmin.wait().catch((error: unknown) => error);
          assertEquals(
            (await nativeAdmin.deploymentsGet({
              deploymentId: nativeKey.deploymentId,
            }).orThrow()).deployment.deploymentId,
            nativeKey.deploymentId,
          );
          const nativeSibling = await TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.AdminService.participant,
            name: "native-admin-sibling",
            seed: nativeSiblingKey.seed,
          }).orThrow();
          const nativeSiblingExit = nativeSibling.wait().catch(
            (error: unknown) => error,
          );
          try {
            await nativeAdmin.grantsRevoke({
              ownerKind: deniedBinding.ownerKind,
              ownerId: deniedBinding.ownerId,
              participantId: deniedBinding.participantId,
              expectedRevision: deniedBinding.revision,
              idempotencyKey: crypto.randomUUID(),
              reason: "native administrator acceptance",
            }).orThrow();
            await nativeSibling.grantsGet({
              ownerKind: nativeBinding.ownerKind,
              ownerId: nativeBinding.ownerId,
              participantId: nativeBinding.participantId,
            }).orThrow();
            const disabled = await admin.serviceInstancesDisable({
              instanceId: nativeKey.instanceId,
              expectedVersion: 1n,
              idempotencyKey: crypto.randomUUID(),
              reason: "instance isolation acceptance",
            }).orThrow();
            assertEquals(disabled.instance.state, "disabled");
            await nativeAdminExit;
            const disabledReconnect = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.AdminService.participant,
              name: "disabled-native-admin",
              seed: nativeKey.seed,
            });
            assert(disabledReconnect.isErr());
            await nativeSibling.grantsGet({
              ownerKind: nativeBinding.ownerKind,
              ownerId: nativeBinding.ownerId,
              participantId: nativeBinding.participantId,
            }).orThrow();
          } finally {
            await nativeAdmin.stop();
            await nativeSibling.stop();
            const siblingError = await nativeSiblingExit;
            assert(
              !(siblingError instanceof Error),
              siblingError instanceof Error ? siblingError.message : undefined,
            );
          }
          const sessions = await consoleClient.sessionsList({
            participantId: participants.Caller.participant.id,
            state: "active",
          }).orThrow();
          assertEquals(sessions.items.length, 1);
          const session = sessions.items[0];
          await admin.sessionsRevoke({
            sessionId: session.sessionId,
            expectedVersion: session.version,
            idempotencyKey: crypto.randomUUID(),
            reason: "acceptance",
          }).orThrow();
          await runtime.waitFor(
            () => ["closed", "error"].includes(client.connection.status.phase),
          );
          const revoked = await client.echo({ value: "revoked" }, {
            timeout: 1000,
          });
          assert(revoked.isErr());
          assert(revoked.error instanceof TransportError);
          assertEquals(revoked.error.code, "trellis.request.closed");
        },
      );
    } finally {
      await service.stop();
      const error = await serviceExit;
      assertEquals(error, undefined);
    }
  });
});

Deno.test("surviving replica reclaims an expired operation lease", async () => {
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
        }).orThrow()
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
        return await op.complete({ value: `reclaimed ${input.value}` })
          .orThrow();
      });
    }
    const client = await runtime.connectClient({
      name: "operation-failover-caller",
      contract: participants.Caller.participant,
    });
    const exits = services.map((service) => service.wait());
    try {
      const operation = await client.work({ value: "lease" }).start().orThrow();
      const owner = await started.promise;
      await services[owner].stop();
      await exits[owner];
      const terminal = await operation.wait().orThrow();
      assertEquals(terminal.state, "completed");
      assertEquals(terminal.output?.value, "reclaimed lease");
      assertEquals(executions, 2);
    } finally {
      await client.connection.close();
      await Promise.all(services.map((service) => service.stop()));
      await Promise.all(exits);
    }
  });
});

Deno.test("parallel client and service shutdown is repeatable", async () => {
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
            contract: participants.Caller.participant,
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
