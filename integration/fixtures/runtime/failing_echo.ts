// A TypeScript service whose Echo handler, Work Operation and keyed job fail,
// for tests that read each failure's log line from this process's output.
// Echo fails on "fail"; Echo "job" starts a keyed job its worker fails and
// answers the job's ID; Work fails on "fail".
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/runtime-trellis/index.js";

const [trellisUrl, seed] = process.argv.slice(2);
if (!trellisUrl || !seed)
  throw new Error("usage: failing_echo.ts <url> <seed>");

const CAUSE = "the fixture's store is unreachable";
const service = await TrellisService.connect({
  trellisUrl,
  seed,
  name: "failing-echo",
  participant: participants.Provider.participant,
}).orThrow();
service.jobs.keyedWork.handle(() => {
  throw new Error(`job failed: ${CAUSE}`);
});
await service.handleEcho(async ({ input }) => {
  if (input.value === "fail") {
    throw new Error(`echo failed: ${CAUSE}`);
  }
  if (input.value === "job") {
    const job = await service.jobs.keyedWork
      .create({ key: "failing", value: "fail" })
      .orThrow();
    return Result.ok({ value: job.id });
  }
  return Result.ok(input);
});
await service.handleWork(() => {
  throw new Error(`work failed: ${CAUSE}`);
});
process.stderr.write("READY\n");
await service.wait();
