import {
  StoreError,
  TransferGrantSchema,
  TrellisClient,
} from "@oatscenter/trellis";
import { Value } from "typebox/value";
import { sha256 } from "@noble/hashes/sha256";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { ensureTelemetryRuntime } from "@oatscenter/trellis/telemetry";
import { err, isErr, ok } from "@oatscenter/result";
import { completeLocalAuthFlow } from "../../ts/packages/trellis-testkit/src/admin/auth_flow.ts";
import { participants, types } from "./packages/performance-trellis/index.js";
import { AuthError } from "./packages/performance-trellis/apis/auth/mod.js";
import {
  deadline,
  digest,
  payload,
  type Sample,
  summarize,
  WorkerOptions,
} from "./model.ts";
import { snapshotResources } from "./resources.ts";

const options = WorkerOptions.parse(
  JSON.parse(await new Response(Deno.stdin.readable).text()),
);
const telemetry = await ensureTelemetryRuntime({
  serviceName: `trellis-benchmark-${options.role}`,
  role: options.role === "provider" ? "service" : "cli",
});
let activePhase = "";
const phase = async (name: string) => {
  if (name !== activePhase) {
    activePhase = name;
    await Deno.writeTextFile(`${options.output}/phase.txt`, name);
  }
};

if (options.role === "provider") {
  const started = performance.now();
  const service = await TrellisService.connect({
    trellisUrl: options.trellisUrl,
    name: `benchmark-provider-${options.providerIndex}`,
    participant: participants.Provider.participant,
    seed: options.serviceSeed!,
    runtime: {
      requestLimits: {
        requests: options.requestLimit,
        bytes: options.requestByteLimit,
      },
      maxVerificationWorkers: options.maxVerificationWorkers,
    },
  }).orThrow();
  const connectMs = performance.now() - started;
  const store = await service.store.files.open().orThrow();
  for (const size of options.sizes) {
    const body = payload(size);
    await store.put(String(size), body).orThrow();
    await Deno.writeFile(`${options.output}/body-${size}.bin`, body);
  }
  if (options.workload === "transfer") {
    // Runtime-private diagnostic: only the ordinary storage-neutral store API.
    const rows: Sample[] = [];
    for (const size of options.sizes) {
      const body = payload(size);
      const expected = await digest(body);
      for (let index = -options.warmups; index < options.samples; index++) {
        for (const action of ["write", "read"] as const) {
          const row: Sample = {
            scenario: `backend-${action}`,
            transport: "store",
            bytes: size,
            warmup: index < 0,
            startedUnixMs: Date.now(),
            durationMs: 0,
          };
          const started = performance.now();
          try {
            if (action === "write") {
              await store.put(
                `diagnostic-${size}`,
                (async function* () {
                  yield body;
                })(),
              ).orThrow();
              row.durationMs = performance.now() - started;
              // Read-back is mandatory but outside the isolated write timing.
            }
            const entry = await store.get(`diagnostic-${size}`).orThrow();
            const stream = await entry.stream().orThrow();
            const hash = sha256.create();
            let length = 0;
            for await (const chunk of stream) {
              length += chunk.length;
              hash.update(chunk);
            }
            const actual = Array.from(
              hash.digest(),
              (byte) => byte.toString(16).padStart(2, "0"),
            ).join("");
            if (length !== size || actual !== expected) {
              throw new Error("Corrupt backend diagnostic round trip");
            }
            if (action === "read") row.durationMs = performance.now() - started;
          } catch (error) {
            row.durationMs = performance.now() - started;
            row.error = Deno.inspect(error, { colors: false, depth: 6 });
          }
          rows.push(row);
        }
      }
      await store.delete(`diagnostic-${size}`).orThrow();
    }
    await Deno.writeTextFile(
      `${options.output}/store-samples.json`,
      JSON.stringify(rows, null, 2),
    );
  }
  await service.handleEcho(async ({ input }) => {
    if (options.rpcDelayMs) {
      await new Promise((resolve) => setTimeout(resolve, options.rpcDelayMs));
    }
    return ok(input);
  });
  await service.handlePutRecord(async ({ input }) => {
    await service.kv.records.put(input.value, input).orThrow();
    if (options.rpcDelayMs) {
      await new Promise((resolve) => setTimeout(resolve, options.rpcDelayMs));
    }
    return ok(input);
  });
  await service.handleReadRecord(async ({ input }) => {
    const value = await service.kv.records.get(input.value).orThrow();
    if (!value) {
      throw new Error("Benchmark record missing after acknowledged write");
    }
    return ok(value);
  });
  await service.handleDownload(async ({ input, context }) => {
    if (context.caller.type !== "verified") {
      throw new Error("Download requires a verified RPC caller");
    }
    const transfer = await service.createTransfer({
      direction: "receive",
      store: "files",
      key: input.value,
      sessionKey: context.caller.sessionKey,
      connectionId: context.caller.connectionId,
      contextDigest: context.caller.contextDigest,
      permission: context.permission,
      requiredCapabilities: context.requiredCapabilities,
      inboxPrefix: context.inboxPrefix,
    }).orThrow();
    return ok({ ...input, transfer });
  });
  await service.handleUpload(async ({ input, op, transfer }) => {
    if (!transfer) {
      throw new Error("Upload arrived without its supported transfer");
    }
    const body = await transfer.stream().orThrow();
    await store.put(`upload-${input.value}`, body).orThrow();
    const stored = await store.get(`upload-${input.value}`).orThrow();
    if (!stored) throw new Error("Persisted upload is missing");
    return await op.complete({
      value: await digest(await stored.bytes().orThrow()),
    }).orThrow();
  });
  await service.handleWork(async ({ input, op }) => {
    await op.progress(input).orThrow();
    return await op.complete(input).orThrow();
  });
  await service.handleQueueWork(async ({ input, op }) => {
    const job = await service.jobs.work.create(input).orThrow();
    const result = await job.wait().orThrow();
    if (result.state !== "completed" || result.result?.value !== input.value) {
      throw new Error("Private job did not complete correctly");
    }
    return await op.complete(result.result).orThrow();
  });
  await service.handleAwaitCancellation(async ({ input, op, signal }) => {
    await op.progress(input).orThrow();
    if (!signal.aborted) {
      await new Promise<void>((resolve) =>
        signal.addEventListener("abort", () => resolve(), { once: true })
      );
    }
  });
  await service.handleAwaitObject(async ({ input, op }) => {
    const job = await service.jobs.waitForObject.create(input).orThrow();
    const result = await job.wait().orThrow();
    if (result.state !== "completed" || !result.result) {
      throw new Error("Object-dependent job did not complete");
    }
    return await op.complete(result.result).orThrow();
  });
  await service.handleQueueKeyed(async ({ input, op }) => {
    const job = await service.jobs.keyedWork.create(input).orThrow();
    const result = await job.wait().orThrow();
    if (result.state !== "completed" || result.result?.value !== input.value) {
      throw new Error(
        `Contended job did not complete correctly: ${JSON.stringify(result)}`,
      );
    }
    return await op.complete(result.result).orThrow();
  });
  await service.handleWatch(async ({ input, emit }) => {
    if (options.workload === "frames") {
      for (let index = 0; index < options.calls; index++) {
        await emit({ value: `${index}:${input.value}` }).orThrow();
      }
    } else await emit(input).orThrow();
  });
  service.jobs.work.handle(({ job }) => Promise.resolve(ok(job.payload)));
  service.jobs.keyedWork.handle(({ job }) => Promise.resolve(ok(job.payload)), {
    concurrency: 10,
  });
  service.jobs.waitForObject.handle(async ({ job }) => {
    const object = await store.get(job.payload.value).take();
    if (isErr(object)) {
      if (
        !(object.error instanceof StoreError) ||
        object.error.getContext().reason !== "not_found"
      ) throw object.error;
      await service.publishChanged(job.payload).orThrow();
      return err(new RetryJobError());
    }
    return ok({ value: await digest(await object.bytes().orThrow()) });
  });
  await service.onChanged(
    async ({ event }) => {
      await service.publishDelivered(event).orThrow();
      return ok(undefined);
    },
    undefined,
    { mode: options.workload === "frames" ? "ephemeral" : "durable" },
  )
    .orThrow();
  const serving = service.wait();
  let reconnects = 0;
  const unsubscribe = options.workload === "lifecycle"
    ? service.connection.subscribe((status) => {
      if (
        status.phase === "connected" && status.transport?.event === "reconnect"
      ) reconnects++;
      const path =
        `${options.output}/provider-${options.providerIndex}-connection.json`;
      Deno.writeTextFileSync(
        `${path}.pending`,
        JSON.stringify({
          connected: status.phase === "connected",
          reconnects,
        }),
      );
      Deno.renameSync(`${path}.pending`, path);
    })
    : () => {};
  await Deno.writeTextFile(
    `${options.output}/provider-${options.providerIndex}.json`,
    JSON.stringify({
      pid: Deno.pid,
      connectMs,
      storage: "JetStream object store",
    }),
  );
  const stopped = Promise.withResolvers<void>();
  Deno.addSignalListener("SIGTERM", () => stopped.resolve());
  try {
    await stopped.promise;
  } finally {
    unsubscribe();
    await service.stop();
    await serving;
    await telemetry.shutdown();
  }
} else {
  const samples: Sample[] = [];
  const windows: Array<
    {
      scenario: string;
      transport: Sample["transport"];
      calls: number;
      durationMs: number;
      bytes?: number;
      sessions?: number;
      before: Awaited<ReturnType<typeof snapshotResources>>;
      after: Awaited<ReturnType<typeof snapshotResources>>;
    }
  > = [];
  const held: Awaited<ReturnType<typeof connect>>[] = [];
  let seedIndex = 0;
  async function connect(
    seed = options.seeds[seedIndex++],
    sessionId?: string,
  ) {
    if (!seed) throw new Error("Insufficient pre-provisioned keys");
    return await TrellisClient.connect({
      trellisUrl: options.trellisUrl,
      name: "benchmark-client",
      participant: participants.Caller.participant,
      auth: {
        mode: "session_key",
        sessionKeySeed: seed,
        sessionId,
        redirectTo: `${options.trellisUrl}/_trellis/test/client-auth`,
      },
      onAuthRequired: (context) =>
        completeLocalAuthFlow({
          trellisUrl: options.trellisUrl,
          loginUrl: context.loginUrl,
          password: options.password,
        }),
    }).orThrow();
  }
  async function measure(
    scenario: string,
    transport: Sample["transport"],
    action: () => Promise<Partial<Sample> | void>,
    bytes?: number,
    offeredAt?: number,
    warmup = false,
  ) {
    if (scenario !== "echo" && scenario !== "admission-cancel") {
      await phase(`${transport}/${scenario}/${bytes ?? 0}`);
    }
    const before = scenario === "echo" || warmup
      ? undefined
      : await snapshotResources(options.output, options.cpuTicks);
    const startedUnixMs = Date.now();
    const started = performance.now();
    let detail: Partial<Sample> | void = undefined;
    try {
      detail = await action();
      samples.push({
        scenario,
        transport,
        startedUnixMs,
        durationMs: performance.now() - (offeredAt ?? started),
        ...(offeredAt === undefined ? {} : {
          offeredUnixMs: startedUnixMs - (started - offeredAt),
          schedulerDelayMs: Math.max(0, started - offeredAt),
        }),
        bytes,
        ...detail,
        warmup,
      });
    } catch (error) {
      samples.push({
        scenario,
        transport,
        startedUnixMs,
        durationMs: performance.now() - (offeredAt ?? started),
        ...(offeredAt === undefined ? {} : {
          offeredUnixMs: startedUnixMs - (started - offeredAt),
          schedulerDelayMs: Math.max(0, started - offeredAt),
        }),
        bytes,
        error: Deno.inspect(error, { colors: false, depth: 6 }),
        warmup,
      });
      if (error instanceof AuthError) {
        console.error(
          "BENCHMARK_AUTH_ERROR",
          JSON.stringify(error.toSerializable()),
        );
      }
      // Independent workloads continue, but the complete run remains failed.
    }
    if (before) {
      windows.push({
        scenario,
        transport,
        calls: 1,
        bytes,
        durationMs: performance.now() - started,
        sessions: detail?.sessions,
        before,
        after: await snapshotResources(options.output, options.cpuTicks),
      });
    }
  }
  try {
    if (options.workload === "all") {
      for (let index = 0; index < options.samples; index++) {
        await measure("fresh-connection-first-get", "http", async () => {
          const http = Deno.createHttpClient({ http1: true, http2: false });
          try {
            const request = {
              client: http,
              headers: { "accept-encoding": "identity" },
              signal: AbortSignal.timeout(30_000),
            };
            const reply = await fetch(
              `${options.httpUrl}/echo?value=first-get`,
              request,
            );
            if (!reply.ok || (await reply.json()).value !== "first-get") {
              throw new Error("Fresh HTTP GET failed");
            }
          } finally {
            http.close();
          }
        });
        const seed = options.seeds[seedIndex++];
        const cycle: { client?: Awaited<ReturnType<typeof connect>> } = {};
        await measure(
          index === 0
            ? "first-process-login-first-rpc"
            : "fresh-login-first-rpc",
          "trellis",
          async () => {
            const started = performance.now();
            const client = await connect(seed);
            cycle.client = client;
            held.push(client);
            const connected = performance.now();
            const response = await client.echo({ value: `first-${index}` })
              .orThrow();
            const finished = performance.now();
            if (response.value !== `first-${index}`) {
              throw new Error("Incorrect first RPC response");
            }
            return {
              durationMs: finished - started,
              connectMs: connected - started,
              firstRpcMs: finished - connected,
            };
          },
        );
        if (cycle.client) {
          const sessionId = (await cycle.client.sessionsMe({}).orThrow())
            .session
            ?.sessionId;
          if (!sessionId) {
            throw new Error(
              "Fresh login did not create a public login session",
            );
          }
          await cycle.client.connection.close();
          held.pop();
          await measure("resume-first-rpc", "trellis", async () => {
            const resumeStarted = performance.now();
            const resumed = await connect(seed, sessionId);
            cycle.client = resumed;
            held.push(resumed);
            const resumedAt = performance.now();
            const reply = await resumed.echo({ value: "resumed" }).orThrow();
            const repliedAt = performance.now();
            if (reply.value !== "resumed") {
              throw new Error("Resume did not reach the provider");
            }
            return {
              durationMs: repliedAt - resumeStarted,
              connectMs: resumedAt - resumeStarted,
              firstRpcMs: repliedAt - resumedAt,
            };
          });
          await measure("logout", "trellis", async () => {
            await cycle.client!.logout();
          });
          await cycle.client.connection.close();
          held.pop();
        }
      }
    }
    const client = await connect();
    held.push(client);
    if (options.workload === "frames") {
      const value = "x".repeat(options.rpcValueBytes);
      const pending = new Map<
        string,
        ReturnType<typeof Promise.withResolvers<void>>
      >();
      await client.onDelivered((event) => {
        const actual = types.ValueCodec.decode(event).value;
        const waiter = pending.get(actual);
        if (!waiter) {
          console.error(
            "FRAME_BENCHMARK_EVENT_UNEXPECTED",
            JSON.stringify({
              received: actual.slice(0, 80),
              expected: [...pending.keys()].map((value) => value.slice(0, 40)),
            }),
          );
          throw new Error(
            "Event payload corrupted or delivered more than once",
          );
        }
        pending.delete(actual);
        waiter.resolve();
        return ok(undefined);
      }, { mode: "ephemeral" }).orThrow();
      for (let sample = 0; sample < options.samples; sample++) {
        for (const sessions of options.sessionCounts) {
          await measure(
            options.liveRpcProbes
              ? "live-frame-throughput-with-rpc"
              : "live-frame-throughput",
            "trellis",
            async () => {
              const cancellation = await client.awaitCancellation({
                value: "frame-load-control",
              }).start().orThrow();
              let probe: Promise<number> | undefined;
              let firstByteMs: number | undefined;
              let total = 0;
              const started = performance.now();
              const startedUnixMs = Date.now();
              let rpc: Promise<void> | undefined;
              let rpcFailure: unknown;
              let offered = 0;
              const rpcTimer = options.liveRpcProbes
                ? setInterval(() => {
                  const now = performance.now();
                  const due = Math.floor((now - started) / 20);
                  while (offered < due) {
                    const scheduled = ++offered * 20;
                    const schedulerDelayMs = now - started - scheduled;
                    const row = {
                      scenario: "live-competing-rpc",
                      transport: "trellis" as const,
                      sessions,
                      startedUnixMs: startedUnixMs + scheduled,
                      schedulerDelayMs,
                    };
                    if (schedulerDelayMs >= 20 || rpc) {
                      samples.push({
                        ...row,
                        durationMs: 0,
                        loadGeneratorDrop: true,
                      });
                      continue;
                    }
                    const rpcStarted = performance.now();
                    rpc = (async () => {
                      try {
                        const result = await client.echo({
                          value: "live-foreground-probe",
                        }).orThrow();
                        if (
                          result.value !== "live-foreground-probe"
                        ) {
                          throw new Error("Foreground RPC payload corrupted");
                        }
                        samples.push({
                          ...row,
                          durationMs: performance.now() - rpcStarted,
                        });
                      } catch (cause) {
                        rpcFailure ??= cause;
                        samples.push({
                          ...row,
                          durationMs: performance.now() - rpcStarted,
                          error: String(cause),
                        });
                      } finally {
                        rpc = undefined;
                      }
                    })();
                  }
                }, 20)
                : undefined;
              try {
                await deadline(
                  Promise.all(Array.from({ length: sessions }, async () => {
                    const feed = await client.watch({ value }).orThrow();
                    let received = 0;
                    try {
                      for await (const frame of feed) {
                        if (frame.value !== `${received}:${value}`) {
                          throw new Error("Live payload or order corrupted");
                        }
                        received++;
                        total++;
                        firstByteMs ??= performance.now() - started;
                        if (
                          !probe && received >= Math.ceil(options.calls / 2)
                        ) {
                          const requested = performance.now();
                          probe = cancellation.cancel().orThrow().then(() =>
                            performance.now() - requested
                          );
                          void probe.catch(() => {});
                        }
                      }
                      if (received !== options.calls) {
                        throw new Error(
                          `Lost Live frames: ${received}/${options.calls}`,
                        );
                      }
                    } finally {
                      await feed.close();
                    }
                  })),
                );
              } finally {
                clearInterval(rpcTimer);
                await rpc;
              }
              if (rpcFailure) throw rpcFailure;
              if (!probe) throw new Error("Cancellation probe was not sent");
              const cancellationMs = await probe;
              const terminal = await cancellation.wait().orThrow();
              if (terminal.state !== "cancelled") {
                throw new Error("Operation did not cancel under frame load");
              }
              return {
                dataFrames: total,
                operations: total,
                sessions,
                firstByteMs,
                cancellationMs,
              };
            },
            options.rpcValueBytes,
          );
        }
        await measure("event-verified-roundtrip", "trellis", async () => {
          try {
            // Bounded batches avoid retaining all completed request promises and
            // do not silently hide failures as generator drops.
            for (
              let first = 0;
              first < options.calls;
              first += options.maxOutstanding
            ) {
              await deadline(
                Promise.all(
                  Array.from({
                    length: Math.min(
                      options.maxOutstanding,
                      options.calls - first,
                    ),
                  }, async (_, offset) => {
                    const eventValue = `${sample}:${first + offset}:${value}`;
                    const waiter = Promise.withResolvers<void>();
                    pending.set(eventValue, waiter);
                    await client.publishChanged({ value: eventValue })
                      .orThrow();
                    await waiter.promise;
                  }),
                ),
              );
            }
            return { operations: options.calls };
          } catch (cause) {
            throw new Error(
              `Event round trip incomplete; ${pending.size} waiting: ${
                [...pending.keys()].map((value) => value.slice(0, 40)).join(
                  ", ",
                )
              }`,
              { cause },
            );
          } finally {
            pending.clear();
          }
        }, options.rpcValueBytes);
      }
    }
    const ephemeral = new Map<string, () => void>();
    const durable = new Map<string, () => void>();
    if (options.workload === "all") {
      await client.onChanged(
        (event) => {
          ephemeral.get(types.ValueCodec.decode(event).value)?.();
          return ok(undefined);
        },
        { mode: "ephemeral" },
      ).orThrow();
      await client.onDelivered((event) => {
        durable.get(types.ValueCodec.decode(event).value)?.();
        return ok(undefined);
      }, { mode: "ephemeral" }).orThrow();
      for (let index = 0; index < options.samples; index++) {
        const value = `action-${index}`;
        await measure("state-write-read", "trellis", async () => {
          await client.state.saved.set({ value }).orThrow();
          const stored = await client.state.saved.get().orThrow();
          if (stored?.value.value !== value) {
            throw new Error("State round trip lost data");
          }
        });
        await measure("rpc-kv-write-read", "trellis", async () => {
          await client.putRecord({ value }).orThrow();
          const stored = await client.readRecord({ value }).orThrow();
          if (stored.value !== value) {
            throw new Error("KV round trip lost data");
          }
        });
        await measure("operation-progress-complete", "trellis", async () => {
          const operation = await client.work({ value }).start().orThrow();
          const terminal = await operation.wait().orThrow();
          if (
            terminal.state !== "completed" ||
            terminal.output?.value !== value ||
            terminal.progress?.value !== value
          ) throw new Error("Operation outcome or progress was lost");
        });
        await measure("operation-private-job-complete", "trellis", async () => {
          const operation = await client.queueWork({ value }).start().orThrow();
          const terminal = await operation.wait().orThrow();
          if (
            terminal.state !== "completed" || terminal.output?.value !== value
          ) throw new Error("Job-backed operation lost its result");
        });
        await measure("live-start-first-frame", "trellis", async () => {
          const abort = new AbortController();
          try {
            const feed = await client.watch({ value }, { signal: abort.signal })
              .orThrow();
            const received = await deadline(
              feed[Symbol.asyncIterator]().next(),
            );
            if (received.done || received.value.value !== value) {
              throw new Error("Live first frame was lost");
            }
          } finally {
            abort.abort();
          }
        });
        await measure("live-start-first-frame", "http", async () => {
          const response = await fetch(
            `${options.httpUrl}/live?value=${value}`,
            {
              headers: { "accept-encoding": "identity" },
              cache: "no-store",
              signal: AbortSignal.timeout(30_000),
            },
          );
          if (!response.ok || !response.body) {
            throw new Error("HTTP stream failed");
          }
          const reader = response.body.getReader();
          let text = "";
          const decoder = new TextDecoder();
          try {
            while (!text.includes("\n")) {
              const frame = await reader.read();
              if (frame.done) {
                throw new Error("HTTP stream ended before a frame");
              }
              text += decoder.decode(frame.value, { stream: true });
            }
            if (JSON.parse(text.split("\n")[0]).value !== value) {
              throw new Error("HTTP stream returned incorrect data");
            }
          } finally {
            await reader.cancel();
          }
        });
        const seen = Promise.withResolvers<void>();
        const handled = Promise.withResolvers<void>();
        const durableValue = `durable-${value}`;
        ephemeral.set(value, seen.resolve);
        durable.set(durableValue, handled.resolve);
        await measure(
          "event-publish-ephemeral-receive",
          "trellis",
          async () => {
            await client.publishChanged({ value }).orThrow();
            await deadline(seen.promise);
          },
        );
        await measure("event-durable-handler-reply", "trellis", async () => {
          await client.publishChanged({ value: durableValue }).orThrow();
          await deadline(handled.promise);
        });
        ephemeral.delete(value);
        durable.delete(durableValue);
      }
    }
    for (
      let trial = -options.warmups;
      options.workload !== "frames" && trial < options.samples;
      trial++
    ) {
      // Alternate protocols to avoid consistently giving one the warmer host.
      const pair = options.workload === "admission"
        ? ["trellis"] as const
        : trial % 2
        ? ["http", "trellis"] as const
        : ["trellis", "http"] as const;
      for (const transport of pair) {
        const running = new Set<Promise<void>>();
        let attempted = 0;
        let cancellation: Promise<void> | undefined;
        const ready = Promise.withResolvers<void>();
        const operation = options.workload === "admission"
          ? await client.awaitCancellation({ value: `admission-${trial}` })
            .onEvent((event) => {
              if (
                "snapshot" in event &&
                event.snapshot.progress?.value === `admission-${trial}`
              ) ready.resolve();
            })
            .onProgress((event) => {
              if (event.progress.value === `admission-${trial}`) {
                ready.resolve();
              }
            }).start().orThrow()
          : undefined;
        if (operation) await deadline(ready.promise);
        await phase(`${transport}/echo-window`);
        const before = await snapshotResources(
          options.output,
          options.cpuTicks,
        );
        const windowStart = performance.now();
        for (let index = 0; index < options.calls; index++) {
          const offeredAt = options.arrivalRate
            ? windowStart + index * 1000 / options.arrivalRate
            : undefined;
          if (offeredAt !== undefined && offeredAt > performance.now()) {
            await new Promise((resolve) =>
              setTimeout(resolve, offeredAt - performance.now())
            );
          }
          if (operation && index === Math.floor(options.calls / 2)) {
            cancellation = measure(
              "admission-cancel",
              "trellis",
              async () => {
                const result = await operation.cancel().orThrow();
                if (result.state !== "cancelled") {
                  throw new Error("Cancellation did not settle");
                }
              },
              undefined,
              undefined,
              trial < 0,
            );
          }
          if (running.size >= options.maxOutstanding) {
            const now = performance.now();
            samples.push({
              scenario: "echo",
              transport,
              startedUnixMs: Date.now(),
              offeredUnixMs: Date.now() - (now - offeredAt!),
              durationMs: now - offeredAt!,
              schedulerDelayMs: Math.max(0, now - offeredAt!),
              loadGeneratorDrop: true,
              warmup: trial < 0,
              error:
                "Load generator max-outstanding reached; no request was sent",
            });
            continue;
          }
          attempted++;
          const work = measure(
            "echo",
            transport,
            async () => {
              const value = `echo-${index}${"x".repeat(options.rpcValueBytes)}`;
              const response = transport === "trellis"
                ? await client.echo({ value }).orThrow()
                : await (await fetch(`${options.httpUrl}/echo?value=${value}`, {
                  cache: "no-store",
                  headers: { "accept-encoding": "identity" },
                  signal: AbortSignal.timeout(30_000),
                })).json();
              if (response.value !== value) {
                throw new Error("Incorrect echo response");
              }
            },
            undefined,
            offeredAt,
            trial < 0,
          );
          if (!options.arrivalRate) await work;
          else {
            running.add(work);
            void work.then(
              () => running.delete(work),
              () => running.delete(work),
            );
          }
        }
        await Promise.all(running);
        await cancellation;
        if (operation) await operation.stopObserving().orThrow();
        const durationMs = performance.now() - windowStart;
        const after = await snapshotResources(options.output, options.cpuTicks);
        if (trial >= 0) {
          windows.push({
            scenario: "echo",
            transport,
            calls: attempted,
            durationMs,
            before,
            after,
          });
        }
      }
    }
    if (options.workload === "all") {
      for (let index = 0; index < options.samples; index++) {
        await measure("operation-cancel", "trellis", async () => {
          const value = `cancel-${index}`;
          const ready = Promise.withResolvers<void>();
          const started = performance.now();
          const operation = await client.awaitCancellation({ value })
            .onProgress(
              (event) => {
                if (event.progress.value !== value) {
                  throw new Error("Wrong cancellation operation progress");
                }
                ready.resolve();
              },
            ).onEvent((event) => {
              if (
                "snapshot" in event && event.snapshot.progress?.value === value
              ) {
                ready.resolve();
              }
            }).start().orThrow();
          try {
            const finished = operation.wait().orThrow();
            await deadline(
              Promise.race([
                ready.promise,
                finished.then((result) => {
                  throw new Error(
                    `Cancellation target ended before progress: ${
                      JSON.stringify(result)
                    }`,
                  );
                }),
              ]),
              20_000,
            ).catch(async (error: unknown) => {
              console.error(
                "BENCHMARK_CANCEL_NOT_READY",
                JSON.stringify(await operation.get().orThrow()),
              );
              throw error;
            });
            const setupMs = performance.now() - started;
            const cancelStarted = performance.now();
            const result = await deadline(operation.cancel().orThrow(), 25_000)
              .catch(async (error: unknown) => {
                console.error(
                  "BENCHMARK_CANCEL_STATE",
                  JSON.stringify(await operation.get().orThrow()),
                );
                throw error;
              });
            if (result.state !== "cancelled") {
              throw new Error("Operation cancellation did not finish");
            }
            return {
              setupMs,
              cancellationMs: performance.now() - cancelStarted,
            };
          } finally {
            await operation.stopObserving().orThrow();
          }
        });
        await measure("job-key-contention", "trellis", async () => {
          const batch = await Promise.allSettled(
            Array.from({ length: 10 }, async (_, member) => {
              const input = {
                key: "shared",
                value: `batch-${index}-${member}`,
              };
              const operation = await client.queueKeyed(input).start()
                .orThrow();
              const result = await operation.wait().orThrow();
              if (
                result.state !== "completed" ||
                result.output?.value !== input.value ||
                result.output.key !== input.key
              ) {
                throw new Error(
                  `Contended job returned the wrong result for ${
                    JSON.stringify(input)
                  }: ${JSON.stringify(result)}`,
                );
              }
            }),
          );
          const failures = batch.filter((result) =>
            result.status === "rejected"
          );
          if (failures.length) {
            throw new AggregateError(
              failures.map((result) => result.reason),
              "Contended jobs failed",
            );
          }
          return { operations: 10 };
        });
        await measure("job-retry-after-upload", "trellis", async () => {
          const input = { value: `upload-retry-${index}` };
          const missing = Promise.withResolvers<void>();
          ephemeral.set(input.value, missing.resolve);
          const operation = await client.awaitObject(input).start().orThrow();
          try {
            const finished = operation.wait().orThrow();
            await deadline(
              Promise.race([
                missing.promise,
                finished.then((result) => {
                  throw new Error(
                    `Object-dependent operation ended before its missing-object read: ${
                      JSON.stringify(result)
                    }`,
                  );
                }),
              ]),
            );
            const body = payload(1024);
            const upload = await client.upload({ value: `retry-${index}` })
              .transfer(body).start().orThrow();
            const transferred = await upload.wait().orThrow();
            if (transferred.terminal.state !== "completed") {
              throw new Error("Retry dependency upload did not complete");
            }
            const result = await finished;
            if (
              result.state !== "completed" ||
              result.output?.value !== await digest(body)
            ) throw new Error("Retried job did not read the uploaded object");
          } finally {
            ephemeral.delete(input.value);
          }
        });
      }
    }
    for (const size of options.sizes) {
      const body = payload(size);
      const expected = await digest(body);
      for (let index = -options.warmups; index < options.samples; index++) {
        for (
          const transport of index % 2
            ? ["http", "trellis"] as const
            : ["trellis", "http"] as const
        ) {
          await measure(
            "download",
            transport,
            async () => {
              const start = performance.now();
              let stream: ReadableStream<Uint8Array>;
              if (transport === "trellis") {
                const response = await client.download({ value: String(size) })
                  .orThrow();
                const grant = Value.Parse(
                  TransferGrantSchema,
                  response.transfer,
                );
                if (grant.direction !== "receive") {
                  throw new Error("Download returned a send grant");
                }
                stream = await client.transfer(grant).stream()
                  .orThrow();
              } else {
                const response = await fetch(
                  `${options.httpUrl}/download?size=${size}`,
                  {
                    cache: "no-store",
                    headers: { "accept-encoding": "identity" },
                    signal: AbortSignal.timeout(30_000),
                  },
                );
                if (!response.ok || !response.body) {
                  throw new Error(`HTTP download ${response.status}`);
                }
                const encoding = response.headers.get("content-encoding");
                if (encoding && encoding !== "identity") {
                  throw new Error(`Unexpected HTTP compression: ${encoding}`);
                }
                stream = response.body;
              }
              const setupMs = performance.now() - start;
              let firstByteMs: number | undefined;
              const chunks: Uint8Array[] = [];
              let length = 0;
              for await (const chunk of stream) {
                if (chunk.length && firstByteMs === undefined) {
                  firstByteMs = performance.now() - start;
                }
                length += chunk.length;
                chunks.push(chunk);
              }
              const received = new Uint8Array(length);
              let offset = 0;
              for (const chunk of chunks) {
                received.set(chunk, offset);
                offset += chunk.length;
              }
              if (length !== size || await digest(received) !== expected) {
                throw new Error("Corrupt download");
              }
              return { setupMs, firstByteMs };
            },
            size,
            undefined,
            index < 0,
          );
          await measure(
            "upload-persisted",
            transport,
            async () => {
              if (transport === "trellis") {
                const operation = await client.upload({ value: String(size) })
                  .transfer(body).start().orThrow();
                const result = await operation.wait().orThrow();
                if (
                  result.terminal.state !== "completed" ||
                  result.transferred.size !== size
                ) throw new Error("Upload not completed");
                if (
                  result.terminal.state !== "completed" ||
                  result.terminal.output?.value !== expected
                ) {
                  throw new Error("Corrupt persisted upload");
                }
              } else {
                const response = await fetch(
                  `${options.httpUrl}/upload?size=${size}`,
                  {
                    method: "PUT",
                    body,
                    headers: { "accept-encoding": "identity" },
                    signal: AbortSignal.timeout(30_000),
                  },
                );
                if (!response.ok) {
                  throw new Error(`HTTP upload ${response.status}`);
                }
                const stored = await response.json();
                if (stored.size !== size || stored.digest !== expected) {
                  throw new Error("Corrupt persisted upload");
                }
              }
            },
            size,
            undefined,
            index < 0,
          );
        }
      }
    }
    if (options.workload === "all") {
      for (const count of options.sessionCounts) {
        while (held.length < count) held.push(await connect());
        await measure("idle-sessions", "trellis", async () => {
          await phase(`trellis/idle-sessions/${count}`);
          await new Promise((resolve) =>
            setTimeout(resolve, options.idleSeconds * 1000)
          );
          return { sessions: count };
        });
        await phase("idle-validation");
        for (const connection of held) {
          const result = await connection.echo({ value: "idle-alive" })
            .orThrow();
          if (result.value !== "idle-alive") {
            throw new Error("Idle connection stopped working");
          }
        }
      }
    }
  } catch (error) {
    samples.push({
      scenario: "suite-fatal",
      transport: "trellis",
      startedUnixMs: Date.now(),
      durationMs: 0,
      error: Deno.inspect(error, { colors: false, depth: 6 }),
    });
    throw error;
  } finally {
    for (const client of held) await client.connection.close().catch(() => {});
    await phase("trellis/after-disconnect");
    await new Promise((resolve) =>
      setTimeout(resolve, options.idleSeconds * 1000)
    );
    await Deno.writeTextFile(
      `${options.output}/samples.json`,
      JSON.stringify(
        { summary: summarize(samples), samples, windows },
        null,
        2,
      ),
    );
    await telemetry.shutdown();
    if (samples.some((sample) => sample.error)) Deno.exitCode = 1;
  }
}
