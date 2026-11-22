// A TypeScript service whose Echo handler fails on "fail", for tests that read
// the failure's log line from this process's own output.
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/runtime-trellis/index.js";

const [trellisUrl, seed] = process.argv.slice(2);
if (!trellisUrl || !seed)
  throw new Error("usage: failing_echo.ts <url> <seed>");

const service = await TrellisService.connect({
  trellisUrl,
  seed,
  name: "failing-echo",
  participant: participants.Provider.participant,
}).orThrow();
await service.handleEcho(({ input }) => {
  if (input.value === "fail") {
    throw new Error("echo failed: the fixture's store is unreachable");
  }
  return Result.ok(input);
});
process.stderr.write("READY\n");
await service.wait();
