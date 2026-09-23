import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/runtime-trellis/index.js";

const [trellisUrl, seed] = Deno.args;
if (!trellisUrl || !seed) throw new Error("usage: events.ts <url> <seed>");

const service = await TrellisService.connect({
  trellisUrl,
  seed,
  name: "events-ts",
  participant: participants.EventService.participant,
}).orThrow();
const stats = {
  attempts: [] as bigint[],
  active: 0n,
  maxActive: 0n,
  successes: [] as string[],
};
const attemptsByValue = new Map<string, number>();
const eventOptions = Deno.env.get("EPHEMERAL") === "true"
  ? { mode: "ephemeral" as const }
  : { mode: "durable" as const, group: "events" };

await service.onAlpha(
  async ({ event }) => {
    stats.attempts.push(BigInt(Date.now()));
    const attempt = (attemptsByValue.get(event.value) ?? 0) + 1;
    attemptsByValue.set(event.value, attempt);
    stats.active++;
    if (stats.active > stats.maxActive) stats.maxActive = stats.active;
    if (event.value === "slow") {
      await new Promise((resolve) => setTimeout(resolve, 600));
    }
    if (event.value === "crash" && Deno.env.get("CRASH") === "true") {
      await new Promise((resolve) => setTimeout(resolve, 60_000));
    }
    const fails = event.value === "fail" ||
      (event.value === "replay" && attempt <= 3);
    stats.active--;
    if (fails) throw new Error("fixture handler failure");
    stats.successes.push(event.value);
  },
  {},
  eventOptions,
).orThrow();

await service.onBeta(() => {}, {}, eventOptions)
  .orThrow();
await service.handleDeliveryStats(() => Result.ok({ ...stats }));
await service.handleObserved(() =>
  Result.ok({ values: [...stats.successes].sort() })
);
await service.handleDropAlpha(() => Result.ok({}));
await service.wait();
