import { renameSync, writeFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { text } from "node:stream/consumers";
import { inspect } from "node:util";
import { StoreError, decodeTransferGrant } from "@oatscenter/trellis";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
  MemoryNativeOAuthCustody,
  type NativeOAuthSession,
} from "@oatscenter/trellis/native";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { ensureTelemetryRuntime } from "@oatscenter/trellis/telemetry";
import { err, isErr, ok } from "@oatscenter/result";
import { approveNativeDeviceCode } from "../../ts/packages/trellis-testkit/src/admin/native_auth.ts";
import { apps, participants } from "./packages/performance-trellis/index.js";
import {
  deadline,
  digest,
  HTTP_MODES,
  payload,
  payloadChunks,
  streamDigest,
  type Sample,
  summarize,
  WorkerOptions,
} from "./model.ts";
import { createHttpClient } from "./http_client.ts";
import { loadHttpTls } from "./http_security.ts";
import { snapshotResources } from "./resources.ts";
import { captureMetrics } from "./metrics.ts";

const options = WorkerOptions.parse(JSON.parse(await text(process.stdin)));
const saveMetrics = captureMetrics(options.output, options.role);
const telemetry = await ensureTelemetryRuntime({
  serviceName: `trellis-benchmark-${options.role}`,
  role: options.role === "client" ? "cli" : "service",
});
let activePhase = "";
const phase = async (name: string) => {
  if (name !== activePhase) {
    activePhase = name;
    await writeFile(`${options.output}/phase.txt`, name);
  }
};

if (options.role !== "client") {
  const started = performance.now();
  const service = await TrellisService.connect({
    trellisUrl: options.trellisUrl,
    name: `benchmark-${options.role}-${options.providerIndex ?? 0}`,
    participant: participants.Provider.participant,
    seed: options.serviceSeed!,
  }).orThrow();
  const connectMs = performance.now() - started;
  const store = await service.store.files.open().orThrow();
  if (options.role === "storage") {
    // Runtime-private diagnostic: only the ordinary storage-neutral store API.
    const rows: Sample[] = [];
    for (const size of options.sizes) {
      const expected = (await streamDigest(payloadChunks(size))).digest;
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
              await store
                .put(`diagnostic-${size}`, payloadChunks(size))
                .orThrow();
              row.durationMs = performance.now() - started;
              // Read-back is mandatory but outside the isolated write timing.
            }
            const entry = await store.get(`diagnostic-${size}`).orThrow();
            const stream = await entry.stream().orThrow();
            const hash = createHash("sha256");
            let length = 0;
            for await (const chunk of stream) {
              length += chunk.length;
              hash.update(chunk);
            }
            const actual = hash.digest("hex");
            if (length !== size || actual !== expected) {
              throw new Error("Corrupt backend diagnostic round trip");
            }
            if (action === "read") row.durationMs = performance.now() - started;
          } catch (error) {
            row.durationMs = performance.now() - started;
            row.error = inspect(error, { colors: false, depth: 6 });
          }
          rows.push(row);
        }
      }
      await store.delete(`diagnostic-${size}`).orThrow();
    }
    await writeFile(
      `${options.output}/store-samples.json`,
      JSON.stringify(rows, null, 2),
    );
    await service.stop();
    await telemetry.shutdown();
    await saveMetrics();
    process.exit(rows.some((row) => row.error) ? 1 : 0);
  }
  for (const size of options.sizes) {
    await store.put(String(size), payloadChunks(size)).orThrow();
  }
  await service.handleEcho(({ input }) => ok(input));
  await service.handlePutRecord(async ({ input }) => {
    await service.kv.records.put(input.value, input).orThrow();
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
    const transfer = await service
      .createTransfer({
        direction: "receive",
        store: "files",
        key: input.value,
        sessionKey: context.caller.sessionPublicKey,
        connectionId: context.caller.runtimeId,
        contextDigest: context.caller.authorityDigest,
        permission: context.permission,
        requiredCapabilities: context.requiredCapabilities,
        inboxPrefix: context.inboxPrefix,
      })
      .orThrow();
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
    return await op
      .complete({
        value: (await streamDigest(await stored.stream().orThrow())).digest,
      })
      .orThrow();
  });
  // Stream-to-store: the staged commit is the only durable write; complete
  // with the runtime-verified digest instead of copying and re-reading.
  await service.handleUploadStaged(async ({ op, transfer }) => {
    if (!transfer) {
      throw new Error("Upload arrived without its supported transfer");
    }
    const info = await transfer.completed().orThrow();
    const encoded = info.digest?.replace(/^SHA-256=/, "");
    if (!encoded) throw new Error("Staged upload digest missing");
    return await op
      .complete({ value: Buffer.from(encoded, "base64url").toString("hex") })
      .orThrow();
  });
  // Stream uploads read the caller's bytes as they arrive: `<size>` hashes
  // and discards them (stream to stream); `store:<size>` writes them once
  // into the service store (stream to store).
  await service.handleUploadStream(async ({ input, op, transfer }) => {
    if (!transfer) throw new Error("Stream upload arrived without a transfer");
    const stream = await transfer.stream().orThrow();
    const hash = createHash("sha256");
    if (input.value.startsWith("store:")) {
      await store
        .put(
          `upload-stream-${input.value.slice("store:".length)}`,
          (async function* () {
            for await (const chunk of stream) {
              hash.update(chunk);
              yield chunk;
            }
          })(),
        )
        .orThrow();
    } else {
      for await (const chunk of stream) hash.update(chunk);
    }
    return await op.complete({ value: hash.digest("hex") }).orThrow();
  });
  // Stream downloads serve generated bytes with metadata declared up front.
  const declared = new Map<number, string>();
  for (const size of options.sizes) {
    const { digest: hex } = await streamDigest(payloadChunks(size));
    declared.set(
      size,
      `SHA-256=${Buffer.from(hex, "hex").toString("base64url")}`,
    );
  }
  await service.handleDownloadStream(async ({ input, context }) => {
    if (context.caller.type !== "verified") {
      throw new Error("Download requires a verified RPC caller");
    }
    const size = Number(input.value);
    const digest = declared.get(size);
    if (!digest) throw new Error("Unknown generated download size");
    const transfer = await service
      .createTransfer({
        direction: "receive",
        stream: payloadChunks(size),
        info: {
          key: `generated-${size}`,
          size,
          updatedAt: new Date().toISOString(),
          digest,
          metadata: {},
        },
        sessionKey: context.caller.sessionPublicKey,
        connectionId: context.caller.runtimeId,
        contextDigest: context.caller.authorityDigest,
        permission: context.permission,
        requiredCapabilities: context.requiredCapabilities,
        inboxPrefix: context.inboxPrefix,
      })
      .orThrow();
    return ok({ ...input, transfer });
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
        signal.addEventListener("abort", () => resolve(), { once: true }),
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
    await emit(input).orThrow();
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
      )
        throw object.error;
      await service.publishChanged(job.payload).orThrow();
      return err(new RetryJobError());
    }
    return ok({ value: await digest(await object.bytes().orThrow()) });
  });
  await service
    .onChanged(async ({ event }) => {
      await service.publishDelivered(event).orThrow();
      return ok(undefined);
    })
    .orThrow();
  const serving = service.wait();
  let reconnects = 0;
  const unsubscribe =
    options.workload === "lifecycle"
      ? service.connection.subscribe((status) => {
          if (
            status.phase === "connected" &&
            status.transport?.event === "reconnect"
          )
            reconnects++;
          const path = `${options.output}/provider-${options.providerIndex}-connection.json`;
          writeFileSync(
            `${path}.pending`,
            JSON.stringify({
              connected: status.phase === "connected",
              reconnects,
            }),
          );
          renameSync(`${path}.pending`, path);
        })
      : () => {};
  await writeFile(
    `${options.output}/provider-${options.providerIndex}.json`,
    JSON.stringify({
      pid: process.pid,
      connectMs,
      storage: "JetStream object store",
    }),
  );
  const stopped = Promise.withResolvers<void>();
  process.once("SIGTERM", () => stopped.resolve());
  try {
    await stopped.promise;
  } finally {
    unsubscribe();
    await service.stop();
    await serving;
    await telemetry.shutdown();
    await saveMetrics();
  }
} else {
  const samples: Sample[] = [];
  const tls = options.httpTlsDir
    ? await loadHttpTls(options.httpTlsDir)
    : undefined;
  const http = createHttpClient(options.httpUrl!, tls);
  const windows: Array<{
    scenario: string;
    transport: Sample["transport"];
    calls: number;
    durationMs: number;
    bytes?: number;
    sessions?: number;
    before: Awaited<ReturnType<typeof snapshotResources>>;
    after: Awaited<ReturnType<typeof snapshotResources>>;
  }> = [];
  const held: Awaited<ReturnType<typeof connect>>[] = [];
  let credentialIndex = 0;
  const initializedCredentials = new Set<string>();
  const custodyByCredential = new Map<string, MemoryNativeOAuthCustody>();
  async function connect(
    credentialHandle = options.credentialHandles[credentialIndex++],
  ) {
    if (!credentialHandle) throw new Error("Insufficient credential handles");
    const server = (await discoverNativeOAuth(options.trellisUrl)).orThrow();
    let custody = custodyByCredential.get(credentialHandle);
    if (!custody) {
      custody = new MemoryNativeOAuthCustody();
      custodyByCredential.set(credentialHandle, custody);
    }
    const native = (
      await NativeOAuthClient.open({
        custody,
        scope: {
          issuer: server.issuer,
          instanceId: server.trust.instanceId,
          account: server.trust.account,
          clientId: apps.Caller.request.id,
          credentialHandle,
        },
        trust: server.trust,
      })
    ).orThrow();
    try {
      let session: NativeOAuthSession;
      if (initializedCredentials.has(credentialHandle)) {
        session = (await native.resume()).orThrow();
      } else {
        const device = (
          await native.startDevice(apps.Caller.request)
        ).orThrow();
        await approveNativeDeviceCode({
          issuer: server.issuer,
          clientId: apps.Caller.request.id,
          userCode: device.userCode,
          username: options.username,
          password: options.password,
        });
        session = (await device.complete()).orThrow();
      }
      const client = await connectNativeClient({
        session,
        servers: server.natsServers,
        name: "benchmark-client",
        request: apps.Caller.request,
      }).orThrow();
      initializedCredentials.add(credentialHandle);
      return client;
    } catch (error) {
      await native.close();
      throw error;
    }
  }
  async function measure(
    scenario: string,
    transport: Sample["transport"],
    action: () => Promise<Partial<Sample> | void>,
    bytes?: number,
    offeredAt?: number,
    warmup = false,
  ) {
    if (scenario !== "echo") {
      await phase(`${transport}/${scenario}/${bytes ?? 0}`);
    }
    const before =
      scenario === "echo" || warmup
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
        ...(offeredAt === undefined
          ? {}
          : {
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
        ...(offeredAt === undefined
          ? {}
          : {
              offeredUnixMs: startedUnixMs - (started - offeredAt),
              schedulerDelayMs: Math.max(0, started - offeredAt),
            }),
        bytes,
        error: inspect(error, { colors: false, depth: 6 }),
        warmup,
      });
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
          const reply = await createHttpClient(
            options.httpUrl!,
            tls,
            true,
          ).send("GET", "/echo?value=first-get");
          if (
            reply.statusCode !== 200 ||
            JSON.parse(await text(reply)).value !== "first-get"
          ) {
            throw new Error("Fresh HTTP GET failed");
          }
        });
        const credentialHandle = options.credentialHandles[credentialIndex++];
        const cycle: { client?: Awaited<ReturnType<typeof connect>> } = {};
        await measure(
          index === 0
            ? "first-process-login-first-rpc"
            : "fresh-login-first-rpc",
          "trellis",
          async () => {
            const started = performance.now();
            const client = await connect(credentialHandle);
            cycle.client = client;
            held.push(client);
            const connected = performance.now();
            const response = await client
              .echo({ value: `first-${index}` })
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
          await cycle.client.connection.close();
          held.pop();
          await measure("resume-first-rpc", "trellis", async () => {
            const resumeStarted = performance.now();
            const resumed = await connect(credentialHandle);
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
            await cycle.client!.sessionsLogout({}).orThrow();
          });
          await cycle.client.connection.close();
          held.pop();
        }
      }
    }
    const client = await connect();
    held.push(client);
    const ephemeral = new Map<string, () => void>();
    const durable = new Map<string, () => void>();
    if (options.workload === "all") {
      await client
        .onChanged(
          (event) => {
            ephemeral.get(event.value)?.();
            return ok(undefined);
          },
          { mode: "ephemeral" },
        )
        .orThrow();
      await client
        .onDelivered(
          (event) => {
            durable.get(event.value)?.();
            return ok(undefined);
          },
          { mode: "ephemeral" },
        )
        .orThrow();
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
          )
            throw new Error("Operation outcome or progress was lost");
        });
        await measure("operation-private-job-complete", "trellis", async () => {
          const operation = await client.queueWork({ value }).start().orThrow();
          const terminal = await operation.wait().orThrow();
          if (
            terminal.state !== "completed" ||
            terminal.output?.value !== value
          )
            throw new Error("Job-backed operation lost its result");
        });
        await measure("live-start-first-frame", "trellis", async () => {
          const abort = new AbortController();
          try {
            const feed = await client
              .performanceWatch({ value }, { signal: abort.signal })
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
          const response = await http.send("GET", `/live?value=${value}`);
          if (response.statusCode !== 200) {
            response.resume();
            throw new Error("HTTP stream failed");
          }
          let text = "";
          const decoder = new TextDecoder();
          for await (const frame of response as AsyncIterable<Uint8Array>) {
            text += decoder.decode(frame, { stream: true });
            if (text.includes("\n")) break;
          }
          if (!text.includes("\n")) {
            throw new Error("HTTP stream ended before a frame");
          }
          if (JSON.parse(text.split("\n")[0]).value !== value) {
            throw new Error("HTTP stream returned incorrect data");
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
    for (let trial = -options.warmups; trial < options.samples; trial++) {
      // Alternate protocols to avoid consistently giving one the warmer host.
      const pair =
        trial % 2
          ? (["http", "trellis"] as const)
          : (["trellis", "http"] as const);
      for (const transport of pair) {
        await phase(`${transport}/echo-window`);
        const before = await snapshotResources(
          options.output,
          options.cpuTicks,
        );
        const windowStart = performance.now();
        const running = new Set<Promise<void>>();
        const allWork: Promise<void>[] = [];
        let attempted = 0;
        for (let index = 0; index < options.calls; index++) {
          const offeredAt = options.arrivalRate
            ? windowStart + (index * 1000) / options.arrivalRate
            : undefined;
          if (offeredAt !== undefined && offeredAt > performance.now()) {
            await new Promise((resolve) =>
              setTimeout(resolve, offeredAt - performance.now()),
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
              const value = `echo-${index}`;
              const response =
                transport === "trellis"
                  ? await client.echo({ value }).orThrow()
                  : JSON.parse(
                      await text(
                        await http.send("GET", `/echo?value=${value}`),
                      ),
                    );
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
            allWork.push(work);
            void work.then(
              () => running.delete(work),
              () => running.delete(work),
            );
          }
        }
        await Promise.all(allWork);
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
          const operation = await client
            .awaitCancellation({ value })
            .onProgress((event) => {
              if (event.progress.value !== value) {
                throw new Error("Wrong cancellation operation progress");
              }
              ready.resolve();
            })
            .onEvent((event) => {
              if (
                "snapshot" in event &&
                event.snapshot.progress?.value === value
              ) {
                ready.resolve();
              }
            })
            .start()
            .orThrow();
          try {
            const finished = operation.wait().orThrow();
            await deadline(
              Promise.race([
                ready.promise,
                finished.then((result) => {
                  throw new Error(
                    `Cancellation target ended before progress: ${JSON.stringify(
                      result,
                    )}`,
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
            const result = await deadline(
              operation.cancel().orThrow(),
              25_000,
            ).catch(async (error: unknown) => {
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
              const operation = await client
                .queueKeyed(input)
                .start()
                .orThrow();
              const result = await operation.wait().orThrow();
              if (
                result.state !== "completed" ||
                result.output?.value !== input.value ||
                result.output.key !== input.key
              ) {
                throw new Error(
                  `Contended job returned the wrong result for ${JSON.stringify(
                    input,
                  )}: ${JSON.stringify(result)}`,
                );
              }
            }),
          );
          const failures = batch.filter(
            (result) => result.status === "rejected",
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
                    `Object-dependent operation ended before its missing-object read: ${JSON.stringify(
                      result,
                    )}`,
                  );
                }),
              ]),
            );
            const body = payload(1024);
            const upload = await client
              .upload({ value: `retry-${index}` })
              .transfer(body)
              .start()
              .orThrow();
            const transferred = await upload.wait().orThrow();
            if (transferred.terminal.state !== "completed") {
              throw new Error("Retry dependency upload did not complete");
            }
            const result = await finished;
            if (
              result.state !== "completed" ||
              result.output?.value !== (await digest(body))
            )
              throw new Error("Retried job did not read the uploaded object");
          } finally {
            ephemeral.delete(input.value);
          }
        });
      }
    }
    const hashStream = async (
      stream: AsyncIterable<Uint8Array>,
      start: number,
    ) => {
      let firstByteMs: number | undefined;
      const hash = createHash("sha256");
      let length = 0;
      for await (const chunk of stream) {
        if (chunk.length && firstByteMs === undefined) {
          firstByteMs = performance.now() - start;
        }
        length += chunk.length;
        hash.update(chunk);
      }
      return { length, digest: hash.digest("hex"), firstByteMs };
    };
    for (const size of options.sizes) {
      const expected = (await streamDigest(payloadChunks(size))).digest;
      for (let index = -options.warmups; index < options.samples; index++) {
        // Alternate which transport goes first; HTTP modes keep a fixed order.
        for (const transport of index % 2
          ? (["http", "trellis"] as const)
          : (["trellis", "http"] as const)) {
          if (transport === "trellis") {
            for (const scenario of ["download", "download-stream"] as const) {
              await measure(
                scenario,
                "trellis",
                async () => {
                  const start = performance.now();
                  const response = await (
                    scenario === "download"
                      ? client.download({ value: String(size) })
                      : client.downloadStream({ value: String(size) })
                  ).orThrow();
                  const grant = decodeTransferGrant(
                    response.transfer,
                  ).orThrow();
                  if (grant.direction !== "receive") {
                    throw new Error("Download returned a send grant");
                  }
                  const stream = await client
                    .transfer(grant)
                    .stream()
                    .orThrow();
                  const setupMs = performance.now() - start;
                  const received = await hashStream(stream, start);
                  if (
                    received.length !== size ||
                    received.digest !== expected
                  ) {
                    throw new Error("Corrupt download");
                  }
                  return {
                    setupMs,
                    firstByteMs: received.firstByteMs,
                    verifiedMs: performance.now() - start,
                  };
                },
                size,
                undefined,
                index < 0,
              );
            }
            for (const scenario of [
              "upload-persisted",
              "upload-staged",
              "upload-stream",
              "upload-stream-store",
            ] as const) {
              await measure(
                scenario,
                "trellis",
                async () => {
                  const started = performance.now();
                  let payloadSuppliedMs: number | undefined;
                  const body = (async function* () {
                    yield* payloadChunks(size);
                    payloadSuppliedMs = performance.now() - started;
                  })();
                  const operation = await (
                    scenario === "upload-staged"
                      ? client.uploadStaged({ value: String(size) })
                      : scenario === "upload-stream"
                        ? client.uploadStream({ value: String(size) })
                        : scenario === "upload-stream-store"
                          ? client.uploadStream({ value: `store:${size}` })
                          : client.upload({ value: String(size) })
                  )
                    .transfer(body)
                    .start()
                    .orThrow();
                  // Authenticated commit: verified and durably staged.
                  const committedMs = performance.now() - started;
                  const result = await operation.wait().orThrow();
                  if (
                    result.terminal.state !== "completed" ||
                    result.transferred.size !== size ||
                    result.terminal.output?.value !== expected
                  ) {
                    throw new Error("Corrupt persisted upload");
                  }
                  // A stream's commit is verified delivery to the handler; its
                  // durable write, when it has one, completes with the handler.
                  return {
                    payloadSuppliedMs,
                    committedMs,
                    verifiedMs: committedMs,
                    ...(scenario === "upload-stream"
                      ? {}
                      : {
                          durableMs:
                            scenario === "upload-staged"
                              ? committedMs
                              : performance.now() - started,
                        }),
                  };
                },
                size,
                undefined,
                index < 0,
              );
            }
            continue;
          }
          for (const mode of HTTP_MODES) {
            await measure(
              `download-${mode}`,
              "http",
              async () => {
                const start = performance.now();
                const response = await http.send(
                  "GET",
                  `/download/${mode}?size=${size}`,
                );
                if (response.statusCode !== 200) {
                  response.resume();
                  throw new Error(`HTTP download ${response.statusCode}`);
                }
                const encoding = response.headers["content-encoding"];
                if (encoding && encoding !== "identity") {
                  throw new Error(`Unexpected HTTP compression: ${encoding}`);
                }
                const setupMs = performance.now() - start;
                const received = await hashStream(
                  response as AsyncIterable<Uint8Array>,
                  start,
                );
                const declared = response.headers["x-bench-digest"];
                if (
                  received.length !== size ||
                  received.digest !== expected ||
                  (typeof declared === "string" &&
                    declared !== "" &&
                    Buffer.from(
                      declared.replace(/^SHA-256=/, ""),
                      "base64url",
                    ).toString("hex") !== expected)
                ) {
                  throw new Error("Corrupt HTTP download");
                }
                return {
                  setupMs,
                  firstByteMs: received.firstByteMs,
                  verifiedMs: performance.now() - start,
                };
              },
              size,
              undefined,
              index < 0,
            );
            await measure(
              `upload-${mode}`,
              "http",
              async () => {
                const started = performance.now();
                const startedWall = performance.timeOrigin + started;
                let payloadSuppliedMs: number | undefined;
                const chunks = (async function* () {
                  yield* payloadChunks(size, options.httpChunkBytes);
                  payloadSuppliedMs = performance.now() - started;
                })();
                const response = await http.send(
                  "PUT",
                  `/upload/${mode}?size=${size}`,
                  { chunks, length: size },
                );
                const reply = await text(response);
                if (response.statusCode !== 200) {
                  throw new Error(
                    `HTTP upload ${response.statusCode}: ${reply}`,
                  );
                }
                const stored = JSON.parse(reply);
                if (stored.size !== size || stored.digest !== expected) {
                  throw new Error("Corrupt persisted upload");
                }
                return {
                  payloadSuppliedMs,
                  verifiedMs: stored.verifiedUnixMs - startedWall,
                  durableMs: stored.durableUnixMs - startedWall,
                };
              },
              size,
              undefined,
              index < 0,
            );
          }
        }
      }
    }
    if (options.workload === "all") {
      for (const count of options.sessionCounts) {
        while (held.length < count) held.push(await connect());
        await measure("idle-sessions", "trellis", async () => {
          await phase(`trellis/idle-sessions/${count}`);
          await new Promise((resolve) =>
            setTimeout(resolve, options.idleSeconds * 1000),
          );
          return { sessions: count };
        });
        await phase("idle-validation");
        for (const connection of held) {
          const result = await connection
            .echo({ value: "idle-alive" })
            .orThrow();
          if (result.value !== "idle-alive") {
            throw new Error("Idle connection stopped working");
          }
        }
      }
    }
    if (options.workload === "transfer" && options.sizes.length) {
      // Responsiveness probes: Live first frame, or an Echo RPC round trip.
      const probeLive = async (
        scenario: string,
        active?: () => boolean,
        kind: "live" | "rpc" = "live",
      ) => {
        if (kind === "live") await phase(`trellis/${scenario}`);
        const offered = performance.now();
        for (let index = 0; index < 30; index++) {
          const offeredAt = offered + index * 50;
          if (offeredAt > performance.now()) {
            await new Promise((resolve) =>
              setTimeout(resolve, offeredAt - performance.now()),
            );
          }
          if (active && !active()) break;
          const started = performance.now();
          const row: Sample = {
            scenario,
            transport: "trellis",
            startedUnixMs: Date.now(),
            offeredUnixMs: Date.now() - (started - offeredAt),
            schedulerDelayMs: Math.max(0, started - offeredAt),
            durationMs: 0,
          };
          const abort = new AbortController();
          try {
            const value = `${scenario}-${index}`;
            if (kind === "rpc") {
              const reply = await deadline(client.echo({ value }).orThrow());
              if (reply.value !== value) throw new Error("RPC probe lost data");
              continue;
            }
            const feed = await client
              .performanceWatch({ value }, { signal: abort.signal })
              .orThrow();
            const received = await deadline(
              feed[Symbol.asyncIterator]().next(),
            );
            if (received.done || received.value.value !== value)
              throw new Error("Live probe lost its first frame");
          } catch (error) {
            row.error = inspect(error, { colors: false, depth: 6 });
          } finally {
            row.durationMs = performance.now() - offeredAt;
            abort.abort();
            samples.push(row);
          }
        }
      };
      await probeLive("live-first-frame-idle");
      await probeLive("rpc-echo-idle", undefined, "rpc");
      const size = Math.max(...options.sizes);
      const expected = (await streamDigest(payloadChunks(size))).digest;
      await measure(
        "mixed-upload-download",
        "trellis",
        async () => {
          let dataActive = 2;
          const downloadReady = Promise.withResolvers<void>();
          const uploadReady = Promise.withResolvers<void>();
          const finished = Promise.allSettled([
            (async () => {
              try {
                const response = await client
                  .download({ value: String(size) })
                  .orThrow();
                const grant = decodeTransferGrant(response.transfer).orThrow();
                if (grant.direction !== "receive")
                  throw new Error("Download returned a send grant");
                const stream = await client.transfer(grant).stream().orThrow();
                const hash = createHash("sha256");
                let length = 0;
                for await (const chunk of stream) {
                  if (chunk.length) downloadReady.resolve();
                  length += chunk.length;
                  hash.update(chunk);
                }
                if (length !== size || hash.digest("hex") !== expected)
                  throw new Error("Corrupt concurrent download");
              } finally {
                dataActive--;
              }
            })(),
            (async () => {
              let sending = true;
              try {
                const body = (async function* () {
                  for await (const chunk of payloadChunks(size)) {
                    uploadReady.resolve();
                    yield chunk;
                  }
                })();
                const operation = await client
                  .upload({ value: String(size) })
                  .transfer(body)
                  .start()
                  .orThrow();
                dataActive--;
                sending = false;
                const result = await operation.wait().orThrow();
                if (
                  result.transferred.size !== size ||
                  result.terminal.state !== "completed" ||
                  result.terminal.output?.value !== expected
                )
                  throw new Error("Corrupt concurrent persisted upload");
              } finally {
                if (sending) dataActive--;
              }
            })(),
          ]);
          let results: Awaited<typeof finished>;
          try {
            await deadline(
              Promise.race([
                Promise.all([downloadReady.promise, uploadReady.promise]),
                finished.then(() => {
                  throw new Error(
                    "Transfers settled before both sources became active",
                  );
                }),
              ]),
            );
            await Promise.all([
              probeLive(
                "live-first-frame-under-transfer",
                () => dataActive > 0,
              ),
              probeLive("rpc-echo-under-transfer", () => dataActive > 0, "rpc"),
            ]);
            await phase("trellis/mixed-upload-download");
          } finally {
            results = await finished;
          }
          const failures = results.filter(
            (result) => result.status === "rejected",
          );
          if (failures.length)
            throw new AggregateError(
              failures.map((result) => result.reason),
              "Concurrent transfers failed",
            );
          return { operations: 2 };
        },
        2 * size,
      );
    }
  } catch (error) {
    samples.push({
      scenario: "suite-fatal",
      transport: "trellis",
      startedUnixMs: Date.now(),
      durationMs: 0,
      error: inspect(error, { colors: false, depth: 6 }),
    });
    throw error;
  } finally {
    http.close();
    for (const client of held) await client.connection.close().catch(() => {});
    await phase("trellis/after-disconnect");
    await new Promise((resolve) =>
      setTimeout(resolve, options.idleSeconds * 1000),
    );
    await writeFile(
      `${options.output}/samples.json`,
      JSON.stringify(
        { summary: summarize(samples), samples, windows },
        null,
        2,
      ),
    );
    await telemetry.shutdown();
    await saveMetrics();
    if (samples.some((sample) => sample.error)) process.exitCode = 1;
  }
}
