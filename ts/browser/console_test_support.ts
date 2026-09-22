// Shared console acceptance fixtures and assertions. Real runtime and real
// generated client only: never inject authorization or fake an RPC response.

import { assertEquals } from "@std/assert";
import type { Page } from "playwright";
import { TrellisService } from "@qlever-llc/trellis/service";
import type { TrellisTestRuntime } from "@qlever-llc/trellis-test";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
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
  deploymentId: string;
  displayName: string;
};

/** A seeded service plus the handle used to stop it before runtime teardown. */
export type SeededServiceWithResources = SeededService & {
  /** Stops the connected provider so its socket cannot outlive the runtime. */
  stop: () => Promise<void>;
};

/** Creates a service deployment profile with no installed participant. */
export async function seedServiceWithoutParticipant(
  runtime: TrellisTestRuntime,
  label = "no-participant",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  const created = await runtime.callAdminRpc("authDeploymentsCreate", {
    displayName,
    expiresAt: null,
    idempotencyKey: ulid(),
    kind: "service",
    participantId: null,
    portalId: null,
    requiresDeviceDelegation: false,
    reviewMode: null,
  });
  return { deploymentId: created.deployment.deploymentId, displayName };
}

/**
 * Creates and applies a service deployment whose participant declares no
 * material resources.
 */
export async function seedServiceWithoutResources(
  runtime: TrellisTestRuntime,
  label = "no-resources",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  await runtime.deployments.create({ id: displayName, kind: "service" });
  const applied = await runtime.contracts.apply({
    deployment: displayName,
    contract: participants.OperationProvider.participant,
  });
  if (!applied.deploymentId) {
    throw new Error("deployment apply returned no deployment ID");
  }
  return { deploymentId: applied.deploymentId, displayName };
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
  await runtime.deployments.create({ id: displayName, kind: "service" });
  const key = await runtime.registerService({
    name: displayName,
    contract: participants.Provider.participant,
    deployment: displayName,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    name: displayName,
    seed: key.seed,
  }).orThrow();
  void service.wait().catch(() => {});
  await runtime.waitFor(async () => {
    const detail = await runtime.callAdminRpc("authDeploymentsGet", {
      deploymentId: key.deploymentId,
    });
    return detail.resources.length > 0 ? true : undefined;
  }, { timeoutMs: 30_000 });
  return {
    deploymentId: key.deploymentId,
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
  reviewMode: "none" | "required" = "none",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  const created = await runtime.callAdminRpc("authDeploymentsCreate", {
    displayName,
    expiresAt: null,
    idempotencyKey: ulid(),
    kind: "device",
    participantId: null,
    portalId: null,
    requiresDeviceDelegation: false,
    reviewMode: new TextEncoder().encode(JSON.stringify(reviewMode)),
  });
  return { deploymentId: created.deployment.deploymentId, displayName };
}

/**
 * Creates and applies a device deployment with the fixture Device participant,
 * so it can actually be provisioned. Resolves the server-allocated deployment
 * ID through the runtime's own name-to-ID mapping and returns it.
 */
export async function seedProvisionableDeviceDeployment(
  runtime: TrellisTestRuntime,
  label = "device",
  reviewMode: "none" | "required" = "none",
): Promise<SeededService> {
  const displayName = fixtureName(label);
  await runtime.deployments.create({
    id: displayName,
    kind: "device",
    reviewMode,
  });
  // The fixture Device participant has a companion, so the companion
  // definition must be installed before the device can be applied.
  await runtime.contracts.install({
    contract: participants.Device.Companion.participant,
  });
  await runtime.contracts.apply({
    deployment: displayName,
    contract: participants.Device.participant,
  });
  // The runtime resolves the name; the response carries the allocated ID.
  const provisioned = await runtime.devices.provision({
    deploymentId: displayName,
    idempotencyKey: ulid(),
    identityPublicKey: null,
    instanceId: null,
    participantId: null,
  });
  return {
    deploymentId: provisioned.device.deploymentId,
    displayName,
  };
}

/** Registers pageerror/unexpected console.error capture before navigation. */
export function captureBrowserErrors(page: Page): string[] {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  return errors;
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

/** Waits for the route-owned marker after its real reads settle. */
export async function waitForConsolePage(
  page: Page,
  timeoutMs = 60_000,
): Promise<void> {
  await page
    .locator('[data-testid="console-page-ready"]')
    .waitFor({ state: "visible", timeout: timeoutMs });
}

/** Fails when a nominal journey produced a browser error. */
export function assertNoBrowserErrors(errors: readonly string[]): void {
  assertEquals(errors, [], `unexpected browser errors:\n${errors.join("\n")}`);
}
