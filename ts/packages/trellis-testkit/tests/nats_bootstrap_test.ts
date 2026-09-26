import { assertStringIncludes } from "@std/assert";
import { renderNscScript } from "../src/nats_bootstrap.ts";

Deno.test("nsc script quotes outDirs containing shell metacharacters", () => {
  const script = renderNscScript("/tmp/it's/nats");
  assertStringIncludes(script, "WORK_DIR='/tmp/it'\\''s/nats'");
});
