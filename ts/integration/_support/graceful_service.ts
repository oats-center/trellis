/**
 * A real TypeScript Provider process for stop and upgrade tests. Every event
 * it sees is one JSON line on standard output, so a test can follow it.
 *
 * `ROLE=old` holds what it is given: a slow Echo sleeps for the requested
 * milliseconds, a `handoffWork` job runs until cancelled, and a `changes`
 * delivery waits `HOLD_DELIVERY_MS`. It creates one held job at startup.
 * `ROLE=new` finishes all of them at once, slow Echo calls included.
 */
import { Result } from "@oatscenter/trellis";
import { RetryJobError, TrellisService } from "@oatscenter/trellis/service";
import { participants } from "../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

const { TRELLIS_URL, TRELLIS_IDENTITY_SEED, ROLE, HOLD_DELIVERY_MS } =
  process.env;
const old = ROLE === "old";
const emit = (event: string, fields: Record<string, unknown> = {}) =>
  process.stdout.write(
    `${JSON.stringify({ event, role: ROLE, at: Date.now(), ...fields })}\n`,
  );

const service = await TrellisService.connect({
  trellisUrl: TRELLIS_URL!,
  participant: participants.Provider.participant,
  seed: TRELLIS_IDENTITY_SEED!,
}).orThrow();

await service.handleEcho(async ({ input }) => {
  const slow = /^slow:(\d+)$/.exec(input.value);
  if (slow && old) {
    emit("slow-started");
    await new Promise((resolve) => setTimeout(resolve, Number(slow[1])));
    emit("slow-finished");
  }
  return Result.ok({ value: `${ROLE}:${input.value}` });
});
await service.handleWatch(async ({ emit: send, signal }) => {
  emit("live-opened");
  while (!signal.aborted) {
    await send({ value: ROLE! }).orThrow();
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
          setTimeout(resolve, Number(HOLD_DELIVERY_MS ?? 60_000)),
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

const waiting = service.wait();
emit("waiting");
process.stdin.on("data", (data) => {
  if (data.toString().includes("stop")) void service.stop();
});
await waiting.then(
  () => emit("exited"),
  (error: unknown) => emit("failed", { error: String(error) }),
);
process.exit(0);
