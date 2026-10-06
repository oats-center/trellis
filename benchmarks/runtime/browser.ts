import { BaseError, TrellisClient } from "@oatscenter/trellis";
import { z } from "zod";
import { participants } from "./packages/performance-trellis/index.js";
import { type Sample } from "./model.ts";

const config = z.object({
  trellisUrl: z.url(),
  seed: z.string(),
  index: z.number(),
  resume: z.boolean(),
}).parse(await (await fetch("/config" + location.search)).json());
const startedUnixMs = Date.now();
const started = performance.now();
let client: Awaited<ReturnType<typeof connect>> | undefined;
async function connect() {
  return await TrellisClient.connect({
    trellisUrl: config.trellisUrl,
    name: "benchmark-browser",
    participant: participants.Caller.participant,
    auth: {
      mode: "session_key",
      sessionKeySeed: config.seed,
      sessionId: config.resume
        ? sessionStorage.getItem(`session-${config.index}`) ?? undefined
        : undefined,
      redirectTo: location.origin + "/",
    },
    onAuthRequired: async (context) => {
      const response = await fetch("/login", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ loginUrl: context.loginUrl }),
      });
      if (!response.ok) {
        throw new Error(`Real login failed: ${response.status}`);
      }
      return z.object({ status: z.literal("bound"), transactionId: z.string() })
        .parse(await response.json());
    },
  }).orThrow();
}
const sample: Sample = {
  scenario: config.resume
    ? "browser/resume-first-rpc"
    : "browser/fresh-login-first-rpc",
  transport: "trellis",
  startedUnixMs,
  durationMs: 0,
};
try {
  client = await connect();
  const connected = performance.now();
  const value = `browser-${config.index}`;
  const reply = await client.echo({ value }).orThrow();
  if (reply.value !== value) throw new Error("Incorrect browser Echo response");
  const finished = performance.now();
  sample.durationMs = finished - started;
  sample.connectMs = connected - started;
  sample.firstRpcMs = finished - connected;
  sample.navigationReadyMs = finished;
  const navigation = performance.getEntriesByType("navigation")[0];
  if (navigation instanceof PerformanceNavigationTiming) {
    sample.documentTtfbMs = navigation.responseStart - navigation.requestStart;
  }
  sample.transferredBytes = performance.getEntriesByType("resource").filter((
    entry,
  ): entry is PerformanceResourceTiming =>
    entry instanceof PerformanceResourceTiming
  ).reduce(
    (sum, entry) => sum + entry.transferSize,
    navigation instanceof PerformanceNavigationTiming
      ? navigation.transferSize
      : 0,
  );
  const sessionId = (await client.sessionsMe({}).orThrow()).session?.sessionId;
  if (!sessionId) throw new Error("Browser login session missing");
  sessionStorage.setItem(`session-${config.index}`, sessionId);
  if (config.resume) await client.logout();
} catch (error) {
  sample.durationMs = performance.now() - started;
  sample.error = error instanceof BaseError
    ? JSON.stringify(error.toSerializable())
    : String(error);
} finally {
  if (client) await client.connection.close();
  await fetch("/results", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(sample),
  });
  document.body.textContent = sample.error ?? "Benchmark complete";
  document.body.dataset.complete = "true";
}
