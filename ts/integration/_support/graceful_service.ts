/**
 * A real TypeScript Provider process for stop and upgrade tests. Every event
 * it sees is one JSON line on standard output, so a test can follow it.
 *
 * `ROLE=old` holds what it is given: a slow Echo sleeps for the requested
 * milliseconds, a `handoffWork` job runs until cancelled, and a `changes`
 * delivery waits `HOLD_DELIVERY_MS`. It creates one held job at startup.
 * `ROLE=new` finishes all of them at once, slow Echo calls included. An Echo
 * of `hold:<ms>` sleeps that long in either role.
 *
 * `READINESS_FILE`, `READINESS_PORT` and `STOP_TIMEOUT_MS` set the matching
 * runtime options. With `HOLD_SERVE=1` it registers everything and then waits
 * for a `serve` line on standard input before it serves. A `stop` line calls
 * `service.stop()`, and a `create:<value>` line creates a `handoffWork` job.
 */
import { createInterface } from "node:readline";
import { Result } from "@oatscenter/trellis";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

const env = process.env;
const old = env.ROLE === "old";
const emit = (event: string, fields: Record<string, unknown> = {}) =>
  process.stdout.write(
    `${JSON.stringify({ event, role: env.ROLE, at: Date.now(), ...fields })}\n`,
  );
const commands = createInterface({ input: process.stdin });
const served = Promise.withResolvers<void>();

const service = await TrellisService.connect({
  trellisUrl: env.TRELLIS_URL!,
  participant: participants.Provider.participant,
  seed: env.TRELLIS_IDENTITY_SEED!,
  runtime: {
    readiness: {
      ...(env.READINESS_FILE ? { file: env.READINESS_FILE } : {}),
      ...(env.READINESS_PORT
        ? { http: { port: Number(env.READINESS_PORT), host: "127.0.0.1" } }
        : {}),
    },
    ...(env.STOP_TIMEOUT_MS
      ? { stopTimeoutMs: Number(env.STOP_TIMEOUT_MS) }
      : {}),
  },
}).orThrow();
emit("connected");

await service.handleEcho(async ({ input }) => {
  const slow = /^slow:(\d+)$/.exec(input.value);
  if (slow && old) {
    emit("slow-started");
    await new Promise((resolve) => setTimeout(resolve, Number(slow[1])));
    emit("slow-finished");
  }
  const hold = /^hold:(\d+)$/.exec(input.value);
  if (hold) {
    emit("hold-started");
    await new Promise((resolve) => setTimeout(resolve, Number(hold[1])));
  }
  return Result.ok({ value: `${env.ROLE}:${input.value}` });
});
await service.handleWatch(async ({ emit: send, signal }) => {
  emit("live-opened");
  while (!signal.aborted) {
    await send({ value: env.ROLE! }).orThrow();
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  emit("live-cancelled");
});
await service
  .onChanged(
    async ({ event }) => {
      emit("delivery-started", { value: event.value });
      if (old)
        await new Promise((resolve) =>
          setTimeout(resolve, Number(env.HOLD_DELIVERY_MS ?? 60_000)),
        );
      emit("delivery-finished", { value: event.value });
      return Result.ok(undefined);
    },
    {},
    { mode: "durable", group: "changes" },
  )
  .orThrow();
service.jobs.handoffWork.handle(async ({ job }) => {
  emit("job-started", { value: job.payload.value });
  if (old) {
    await new Promise<void>((resolve) =>
      job.signal.addEventListener("abort", () => resolve(), { once: true }),
    );
    emit("job-cancelled", { value: job.payload.value });
    return Result.err(new RetryJobError());
  }
  emit("job-finished", { value: job.payload.value });
  return Result.ok(job.payload);
});
if (old) await service.jobs.handoffWork.create({ value: "held" }).orThrow();
void service.ready.then(() => emit("ready"));

commands.on("line", (line) => {
  if (line === "serve") served.resolve();
  if (line === "stop") void service.stop();
  if (line.startsWith("create:")) {
    const value = line.slice("create:".length);
    void (async () => {
      const created = await service.jobs.handoffWork.create({ value });
      emit(created.isOk() ? "created" : "create-failed", { value });
    })();
  }
});
if (env.HOLD_SERVE === "1") {
  emit("registered");
  await served.promise;
}
const waiting = service.wait();
emit("waiting");
await waiting.then(
  () => emit("exited"),
  (error: unknown) => emit("failed", { error: String(error) }),
);
// Nothing else holds the process open: it ends once the service has stopped.
commands.close();
process.stdin.destroy();
