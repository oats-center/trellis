// Shared console acceptance fixtures and assertions. Real runtime and real
// generated client only: never inject authorization or fake an RPC response.

import assert from "node:assert/strict";
import type { Page } from "playwright";
import { TrellisService } from "@oatscenter/trellis/service";
import { codecs } from "@oatscenter/trellis/generated";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { types } from "../packages/trellis-testkit/trellis/index.js";
import { openConsole } from "./browser_test_support.ts";
/**
 * Opens the console as the harness-created administrator. Ensures the
 * first-administrator bootstrap through the real harness workflow first, so a
 * test that did not seed via `callAdminRpc` still has a real admin account.
 */
export async function openConsoleAsRuntimeAdmin(
  page: Page,
  runtime: TrellisTestRuntime,
): Promise<void> {
  await runtime.ensureAdmin();
  await openConsole(page, runtime, {
    username: runtime.adminUsername,
    password: runtime.adminPassword,
  });
}

/** Per-test prefix so seeded records are locatable without relying on list order. */
export function fixtureName(label: string): string {
  return `ct-${label}-${ulid().toLowerCase().slice(-10)}`;
}

/** Identifiers returned by real seed operations, never guessed. */
export type SeededService = {
  deploymentId: ReturnType<typeof codecs.ulid.decode>;
  displayName: string;
};

/** A seeded service plus the handle used to stop it before runtime teardown. */
export type SeededServiceWithResources = SeededService & {
  /** Stops the connected provider so its socket cannot outlive the runtime. */
  stop: () => Promise<void>;
};

/**
 * Creates and applies a service deployment whose participant declares no
 * material resources.
 */
export async function seedServiceWithoutResources(
  runtime: TrellisTestRuntime,
  label = "no-resources",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  await runtime.contracts.install({
    contract: participants.EchoProvider.participant,
  });
  const created = await runtime.callAdminRpc("authDeploymentsCreate", {
    title: displayName,
    kind: "service",
    participantId: participants.EchoProvider.participant.identity,
    packageEvidence: types.AuthPackageEvidenceCodec.decode(
      participants.EchoProvider.participant.packageEvidence,
    ),
    idempotencyKey: ulid(),
  });
  return { deploymentId: created.deployment.deploymentId, displayName };
}

/**
 * Creates and applies the fixture Provider participant (KV, state, store,
 * event-consumer, and job-queue declarations), connects it through the
 * ordinary service runtime, and waits until the runtime has materialized real
 * provider evidence.
 */ export async function seedServiceWithResources(
  runtime: TrellisTestRuntime,
  label = "resources",
): Promise<SeededServiceWithResources> {
  const displayName = fixtureName(label);
  const key = await runtime.registerService({
    name: displayName,
    contract: participants.Provider.participant,
    deployment: displayName,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.oauthIssuer,
    participant: participants.Provider.participant,
    name: displayName,
    seed: key.seed,
  }).orThrow();
  void service.wait().catch(() => {});
  await runtime.waitFor(
    async () => {
      const detail = await runtime.callAdminRpc("authResourcesList", {
        ownerId: codecs.ulid.decode(key.deploymentId),
      });
      return detail.items.length > 0 ? true : undefined;
    },
    { timeoutMs: 30_000 },
  );
  return {
    deploymentId: codecs.ulid.decode(key.deploymentId),
    displayName,
    // Browser tests must stop this before their context closes; otherwise the
    // live provider socket reconnects against a dead runtime and surfaces an
    // uncaught transport reset in an unrelated test.
    stop: () => service.stop(),
  };
}

/** Creates a device deployment through `Deployments.Create` and returns its real ID. */
export async function seedDeviceDeployment(
  runtime: TrellisTestRuntime,
  label = "device",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  await runtime.contracts.install({
    contract: participants.ChildProvider.participant,
  });
  await runtime.contracts.install({
    contract: participants.Device.participant,
  });
  const created = await runtime.callAdminRpc("authDeploymentsCreate", {
    title: displayName,
    idempotencyKey: ulid(),
    kind: "device",
    participantId: participants.Device.participant.identity,
    packageEvidence: types.AuthPackageEvidenceCodec.decode(
      participants.Device.participant.packageEvidence,
    ),
  });
  return { deploymentId: created.deployment.deploymentId, displayName };
}

/**
 * Captures browser errors except verified standard OAuth DPoP nonce challenges.
 * Token responses pass through unchanged after inspecting the real server body,
 * before automatic navigation can discard Chromium's response-body cache.
 */
export async function captureBrowserErrors(page: Page): Promise<{
  errors: string[];
  pending: Promise<void>[];
}> {
  const errors: string[] = [];
  const pending: Promise<void>[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    // HTTP responses are checked below, where the actual protocol response can
    // distinguish a successful nonce challenge from an unexpected HTTP failure.
    if (
      message.type() === "error" &&
      !/^Failed to load resource: the server responded with a status of \d{3}/.test(
        message.text(),
      )
    ) {
      const url = URL.parse(message.location().url);
      errors.push(`${message.text()}${url ? ` (${url.pathname})` : ""}`);
    }
  });
  await page.route(
    (url) => url.pathname === "/token",
    (route) => {
      const work = (async () => {
        const response = await route.fetch({ maxRedirects: 0 });
        try {
          if (response.status() < 400) return;
          const path = new URL(response.url()).pathname;
          if (
            response.status() === 400 &&
            path === "/token" &&
            response.headers()["dpop-nonce"]
          ) {
            let readFailure: unknown;
            const body: unknown = await response
              .json()
              .catch((cause: unknown) => {
                readFailure = cause;
                return undefined;
              });
            if (
              body !== null &&
              typeof body === "object" &&
              "error" in body &&
              body.error === "use_dpop_nonce"
            )
              return;
            const code =
              body !== null && typeof body === "object" && "error" in body
                ? body.error
                : undefined;
            errors.push(
              `HTTP 400 (${path}); nonce response error=${typeof code === "string" ? code : "unavailable"}; body read=${String(readFailure)}`,
            );
            return;
          }
          errors.push(`HTTP ${response.status()} (${path})`);
        } finally {
          await route.fulfill({ response });
        }
      })();
      pending.push(work);
      return work;
    },
  );
  page.on("response", (response) => {
    const path = new URL(response.url()).pathname;
    // The real token response was classified before being forwarded above.
    if (path !== "/token" && response.status() >= 400) {
      errors.push(`HTTP ${response.status()} (${path})`);
    }
  });
  return { errors, pending };
}

/** Waits for the shell marker emitted only after Me and route authorization. */
export async function waitForConsoleShell(
  page: Page,
  timeoutMs = 60_000,
): Promise<void> {
  await page
    .locator('[data-testid="console-ready"]')
    .waitFor({ state: "visible", timeout: timeoutMs });
}

/** Fails when a nominal journey produced a browser error. */
export async function assertNoBrowserErrors(
  captured: Awaited<ReturnType<typeof captureBrowserErrors>>,
): Promise<void> {
  await Promise.all(captured.pending);
  const { errors } = captured;
  assert.deepEqual(
    errors,
    [],
    `unexpected browser errors:\n${errors.join("\n")}`,
  );
}
