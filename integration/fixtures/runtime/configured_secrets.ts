// A TypeScript ConfiguredService with a settings check and an API-key client
// that fail the ways an author's code can, quoting the key.
//
// The check throws an error quoting the key, and its base64, when the key
// starts with `check-throws-`; refuses `region` with a reason quoting the
// candidate key and the key in effect when the region is `explicit`; throws
// an error that names no secret when the region is `plain`; and refuses
// `region` with a reason longer than Auth keeps when the region is `long`.
// The client's builder throws an error quoting the key, and its base64, when
// the key starts with `build-throws-`. Prints `READY` to stderr once its check
// and client are registered, then serves.
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./packages/runtime-trellis/index.js";

const [trellisUrl, seed] = process.argv.slice(2);
if (!trellisUrl || !seed)
  throw new Error("usage: configured_secrets.ts <url> <seed>");

const service = await TrellisService.connect({
  trellisUrl,
  seed,
  name: "configured-secrets",
  participant: participants.ConfiguredService.participant,
}).orThrow();
const encoded = (key: string) => Buffer.from(key).toString("base64");
await service.config.check((candidate, { reject }) => {
  if (candidate.apiKey.startsWith("check-throws-"))
    throw new Error(
      `the endpoint refused ${candidate.apiKey} (${encoded(candidate.apiKey)})`,
    );
  if (candidate.region === "explicit")
    reject(
      ["region"],
      `region conflicts with key ${candidate.apiKey}, was ${service.config.current.apiKey}`,
    );
  if (candidate.region === "plain")
    throw new Error("region plain is not served");
  if (candidate.region === "long") reject(["region"], "é".repeat(1000));
});
await service.config.derive(["apiKey"], (candidate) => {
  if (candidate.apiKey.startsWith("build-throws-"))
    throw new Error(
      `cannot sign in with ${candidate.apiKey} (${encoded(candidate.apiKey)})`,
    );
  return candidate.apiKey;
});
process.stderr.write("READY\n");
await service.wait();
