import { AuthError, BaseError, TrellisClient } from "@oatscenter/trellis";
import { z } from "zod";
import { apps } from "./packages/performance-trellis/index.js";
import { type Sample } from "./model.ts";

const query = new URLSearchParams(location.search);
const callback =
  query.has("state") && (query.has("code") || query.has("error"));
const attemptSchema = z.object({
  index: z.number().int().nonnegative(),
  resume: z.boolean(),
  startedUnixMs: z.number(),
  started: z.number(),
});
const attempt = callback
  ? attemptSchema.parse(
      JSON.parse(sessionStorage.getItem("benchmark-attempt") ?? "null"),
    )
  : attemptSchema.parse({
      index: Number(query.get("index")),
      resume: query.get("resume") === "true",
      startedUnixMs: Date.now(),
      started: performance.timeOrigin + performance.now(),
    });
if (!callback)
  sessionStorage.setItem("benchmark-attempt", JSON.stringify(attempt));
const config = z
  .object({
    trellisUrl: z.url(),
    clientId: z.string(),
    credentialHandle: z.string(),
  })
  .parse(await (await fetch(`/config?index=${attempt.index}`)).json());
const sample: Sample = {
  scenario: attempt.resume
    ? "browser/resume-first-rpc"
    : "browser/fresh-login-first-rpc",
  transport: "trellis",
  startedUnixMs: attempt.startedUnixMs,
  durationMs: 0,
};
let navigating = false;
let client: Awaited<ReturnType<typeof connect>> | undefined;
async function connect() {
  return await TrellisClient.connect({
    trellisUrl: config.trellisUrl,
    name: "benchmark-browser",
    request: apps.BrowserCaller.request,
    auth: {
      clientId: config.clientId,
      credentialHandle: config.credentialHandle,
      redirectTo: `${location.origin}/callback`,
      loopbackDevelopment: true,
    },
  }).orThrow();
}
try {
  client = await connect();
  const connected = performance.timeOrigin + performance.now();
  const value = `browser-${attempt.index}`;
  const reply = await client.echo({ value }).orThrow();
  if (reply.value !== value) throw new Error("Incorrect browser Echo response");
  const finished = performance.timeOrigin + performance.now();
  sample.durationMs = finished - attempt.started;
  sample.connectMs = connected - attempt.started;
  sample.firstRpcMs = finished - connected;
  sample.navigationReadyMs = performance.now();
  const navigation = performance.getEntriesByType("navigation")[0];
  if (navigation instanceof PerformanceNavigationTiming) {
    sample.documentTtfbMs = navigation.responseStart - navigation.requestStart;
  }
  sample.transferredBytes = performance
    .getEntriesByType("resource")
    .filter(
      (entry): entry is PerformanceResourceTiming =>
        entry instanceof PerformanceResourceTiming,
    )
    .reduce(
      (sum, entry) => sum + entry.transferSize,
      navigation instanceof PerformanceNavigationTiming
        ? navigation.transferSize
        : 0,
    );
  if (attempt.resume) await client.logout();
} catch (error) {
  if (
    error instanceof AuthError &&
    error.reason === "OAuthAuthorizationRequired"
  ) {
    navigating = true;
  } else {
    sample.durationMs =
      performance.timeOrigin + performance.now() - attempt.started;
    sample.error =
      error instanceof BaseError
        ? JSON.stringify(error.toSerializable())
        : String(error);
  }
} finally {
  if (client) await client.connection.close();
  if (!navigating) {
    await fetch("/results", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(sample),
    });
    document.body.textContent = sample.error ?? "Benchmark complete";
    document.body.dataset.complete = "true";
  }
}
