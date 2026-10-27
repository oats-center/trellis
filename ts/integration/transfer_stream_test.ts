import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createInterface } from "node:readline";
import { test } from "node:test";
import { Objm } from "@nats-io/obj";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { spawnTracked } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

const pattern = (size: number) =>
  Uint8Array.from({ length: size }, (_, index) => index % 251);
const sha256 = (bytes: Uint8Array) =>
  `SHA-256=${createHash("sha256").update(bytes).digest("base64url")}`;

type Event = Record<string, unknown>;
// Released by the test once the caller observed the commit; a `late-end`
// handler waits for it before reading end of file.
let lateEnd = Promise.withResolvers<void>();

for (const language of ["rust", "node"] as const) {
  test(
    `stream Operations deliver caller bytes to a running ${language} handler without staging`,
    { timeout: 180_000 },
    async () => {
      await withTrellisRuntime(async (runtime) => {
        const identity = await runtime.registerService({
          name: `stream-${language}`,
          deployment: `stream-${language}`,
          contract: participants.OperationProvider.participant,
        });
        const events: Event[] = [];
        let stop: () => Promise<void>;
        if (language === "rust") {
          const child = spawnTracked(
            "setsid",
            rustFixtureArgv("stream_transfer"),
            {
              env: {
                ...process.env,
                TRELLIS_URL: runtime.oauthIssuer,
                TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
                TRELLIS_IDENTITY_SEED: identity.seed,
              },
            },
          );
          child.stderr.pipe(process.stderr);
          const output = (async () => {
            for await (const line of createInterface({ input: child.stdout }))
              events.push(JSON.parse(line));
          })();
          stop = async () => {
            child.stdin.end("stop\n");
            await child.exited;
            await output;
          };
          await runtime.waitFor(() => {
            if (child.exitCode !== null) throw new Error("provider exited");
            return events.some((event) => event.ready === "provider");
          });
        } else {
          const service = await TrellisService.connect({
            trellisUrl: runtime.oauthIssuer,
            participant: participants.OperationProvider.participant,
            seed: identity.seed,
          }).orThrow();
          await service.handleIngest(async ({ input, op, transfer }) => {
            assert(transfer);
            const stream = await transfer.stream().orThrow();
            assert(
              (await transfer.stream()).isErr(),
              "a stream upload is read once",
            );
            const hash = createHash("sha256");
            let size = 0;
            const late = input.value.startsWith("late-end:")
              ? Number(input.value.slice("late-end:".length))
              : undefined;
            try {
              for await (const chunk of stream) {
                hash.update(chunk);
                if (late !== undefined && size + chunk.length === late) {
                  // Every byte is read; end of file is read only after the
                  // provider committed and released the transfer.
                  await lateEnd.promise;
                }
                if (size === 0) {
                  // The caller is still sending: bytes reach the handler
                  // before the upload completes.
                  events.push({ partial: chunk.length });
                  if (input.value === "fail-after-partial")
                    throw new Error("handler rejected the stream");
                }
                size += chunk.length;
              }
            } catch (error) {
              events.push({ streamError: String(error) });
              throw error;
            }
            const info = await transfer.completed().orThrow();
            const digest = `SHA-256=${hash.digest("base64url")}`;
            assert.equal(info.size, size);
            assert.equal(info.digest?.replace(/=+$/, ""), digest);
            events.push({ streamed: size });
            return await op
              .complete({ value: `node:${size}:${digest}` })
              .orThrow();
          });
          await service.handleGenerate(async ({ input, context }) => {
            assert(context.caller.type === "verified");
            const lie = input.value === "lie";
            const size = lie ? 4096 : Number(input.value);
            const bytes = pattern(size);
            const transfer = await service
              .createTransfer({
                direction: "receive",
                stream: (async function* () {
                  if (bytes.length) yield bytes;
                })(),
                info: {
                  key: "generated",
                  size,
                  updatedAt: new Date().toISOString(),
                  // `lie` declares the digest of different same-sized bytes.
                  digest: sha256(lie ? new Uint8Array(size) : bytes),
                  contentType: "application/octet-stream",
                  metadata: {},
                },
                context,
              })
              .orThrow();
            return Result.ok({ ...input, transfer });
          });
          stop = async () => {
            await service.stop();
            await service.wait();
          };
        }
        const client = await runtime.connectClient({
          name: `stream-${language}-caller`,
          contract: apps.Caller.request,
        });
        const generate = async (value: string) => {
          const response = await client.generate({ value }).orThrow();
          const grant = response.transfer;
          assert(grant);
          return await client.transfer(grant).bytes();
        };
        try {
          // The provider answers once its handlers are subscribed.
          await runtime.waitFor(async () => (await generate("0")).isOk());

          // Direct delivery: the handler reads while the caller is paused.
          // Larger than one 4 MiB / 16-frame window, so credit must flow.
          const bytes = pattern(9 * 1024 * 1024 + 11);
          let release = () => {};
          const paused = new Promise<void>((resolve) => (release = resolve));
          const body = (async function* () {
            yield bytes.subarray(0, 64 * 1024);
            await paused;
            yield bytes.subarray(64 * 1024);
          })();
          const starting = client
            .ingest({ value: "complete" })
            .transfer(body)
            .start()
            .orThrow();
          await runtime.waitFor(() => events.some((event) => event.partial));
          assert(!events.some((event) => event.streamed));
          release();
          const operation = await starting;
          const result = await operation.wait().orThrow();
          assert.equal(result.terminal.state, "completed");
          assert.equal(result.transferred.size, bytes.length);
          assert.equal(
            result.terminal.output?.value,
            `${language}:${bytes.length}:${sha256(bytes)}`,
          );
          // Nothing was staged for the stream.
          const nc = await runtime.connectNats();
          const staging = await new Objm(nc)
            .open(`trellis_operation_staging_${identity.deploymentId}`)
            .catch(() => undefined);
          if (staging) assert.deepEqual(await staging.list(), []);

          if (language === "node") {
            // A handler still busy with its last bytes when the provider
            // commits and releases the transfer still reads end of file.
            lateEnd = Promise.withResolvers<void>();
            const late = pattern(3 * 1024 * 1024 + 5);
            const committed = await client
              .ingest({ value: `late-end:${late.length}` })
              .transfer(late)
              .start()
              .orThrow();
            lateEnd.resolve();
            const finished = await committed.wait().orThrow();
            assert.equal(finished.terminal.state, "completed");
            assert.equal(
              finished.terminal.output?.value,
              `node:${late.length}:${sha256(late)}`,
            );
          }

          // A handler that fails mid-stream fails the Operation and the upload.
          let failedRef: Parameters<typeof client.ingest.resume>[0] | undefined;
          const rejected = await client
            .ingest({ value: "fail-after-partial" })
            .transfer(pattern(2 * 1024 * 1024))
            .onAccepted(({ snapshot }) => {
              failedRef = { ...snapshot, deploymentId: identity.deploymentId };
            })
            .start();
          assert(rejected.isErr(), "an unconsumed stream is not delivered");
          assert(failedRef);
          const failed = await client.ingest.resume(failedRef).wait().orThrow();
          assert.equal(failed.state, "failed");

          // A caller that abandons the upload mid-stream ends the handler's
          // stream with an error instead of end of file.
          let abandonedRef:
            Parameters<typeof client.ingest.resume>[0] | undefined;
          const before = events.filter((event) => event.partial).length;
          const abandoned = await client
            .ingest({ value: "await-error" })
            .transfer(
              (async function* () {
                yield bytes.subarray(0, 64 * 1024);
                await runtime.waitFor(
                  () => events.filter((event) => event.partial).length > before,
                );
                throw new Error("caller abandoned the upload");
              })(),
            )
            .onAccepted(({ snapshot }) => {
              abandonedRef = {
                ...snapshot,
                deploymentId: identity.deploymentId,
              };
            })
            .start();
          assert(abandoned.isErr());
          await runtime.waitFor(() =>
            events.some((event) => event.streamError),
          );
          assert(abandonedRef);
          assert.equal(
            (await client.ingest.resume(abandonedRef).wait().orThrow()).state,
            "failed",
          );

          // Generated downloads stream from the provider's own source, bound
          // to the size and digest it declared.
          const generated = pattern(2 * 1024 * 1024 + 3);
          assert.deepEqual(
            (await generate(String(generated.length))).orThrow(),
            generated,
          );
          assert(
            (await generate("lie")).isErr(),
            "bytes that differ from the declared digest fail integrity",
          );
        } finally {
          await client.connection.close();
          await stop();
        }
      });
    },
  );
}

test("a download source whose cancellation never settles cannot block provider shutdown", async () => {
  await withTrellisRuntime(async (runtime) => {
    const identity = await runtime.registerService({
      name: "stalled-download",
      contract: participants.OperationProvider.participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.oauthIssuer,
      participant: participants.OperationProvider.participant,
      seed: identity.seed,
    }).orThrow();
    const serviceExit = service.wait().catch((error: unknown) => error);
    const stalled = Promise.withResolvers<void>();
    const first = pattern(1024);
    await service.handleGenerate(async ({ input, context }) => {
      assert(context.caller.type === "verified");
      const transfer = await service
        .createTransfer({
          direction: "receive",
          // Application code that misbehaves twice: it keeps producing past
          // the size it declared, failing the download, and its cancel never
          // settles. The failure starts that cancellation before shutdown.
          stream: new ReadableStream<Uint8Array>({
            pull(controller) {
              controller.enqueue(first);
            },
            cancel() {
              stalled.resolve();
              return new Promise<never>(() => {});
            },
          }),
          info: {
            key: "stalled",
            size: first.length,
            updatedAt: new Date().toISOString(),
            digest: sha256(first),
            contentType: "application/octet-stream",
            metadata: {},
          },
          context,
        })
        .orThrow();
      return Result.ok({ ...input, transfer });
    });
    const client = await runtime.connectClient({
      name: "stalled-download-caller",
      contract: apps.Caller.request,
    });
    // The provider answers once its handlers are subscribed.
    await runtime.waitFor(async () =>
      (await client.generate({ value: "ready" })).isOk(),
    );
    const response = await client.generate({ value: "stalled" }).orThrow();
    const grant = response.transfer;
    assert(grant);
    const download = client.transfer(grant).bytes();
    await stalled.promise;

    const stopped = await Promise.race([
      service.stop().then(() => true),
      new Promise<false>((resolve) => setTimeout(() => resolve(false), 20_000)),
    ]);
    assert(stopped, "provider shutdown waited on the stalled download source");
    await serviceExit;
    assert((await download).isErr());
  });
});
