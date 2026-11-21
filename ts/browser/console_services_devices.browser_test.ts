// Console service journeys. Real runtime, real generated client.

import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import nacl from "tweetnacl";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  startedServices,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  openConsoleAsRuntimeAdmin,
  seedDeviceDeployment,
  seedServiceWithResources,
  seedServiceWithoutResources,
  waitForConsoleShell,
  wireNatsGate,
} from "./console_test_support.ts";
import { API as authApi } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";

const fixtureIdl = ["trellis.toml", "contract.trellis"].map((name) =>
  fileURLToPath(
    new URL(`../../integration/fixtures/runtime/${name}`, import.meta.url),
  ),
);

test("B01 adding a service with its IDL and a created key lets the service connect with that key", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.contracts.install({
        contract: participants.EchoProvider.participant,
      });
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);

        const displayName = `ct-b01-${ulid()}`;
        await page.getByLabel("Name", { exact: true }).fill(displayName);
        // The fixture's own trellis.toml and source; Auth resolves its
        // dependency on the trellis package from the packages it ships.
        await page
          .getByLabel("Service IDL (optional)")
          .setInputFiles(fixtureIdl);
        // The fixture IDL declares several services, so the form asks which.
        await page
          .getByLabel("Which service")
          .selectOption(participants.EchoProvider.participant.identity);
        // "Create a key" is the default.
        await page
          .getByRole("button", { name: "Add service", exact: true })
          .click();

        const dialog = page.getByRole("dialog", {
          name: "Save the service's key",
        });
        const seed = (
          await dialog.getByLabel("Service key").textContent()
        )?.trim();
        assert(seed, "the dialog shows the generated key");
        await dialog
          .getByRole("button", { name: "I've saved it", exact: true })
          .click();
        await dialog.waitFor({ state: "hidden" });
        assert.equal(
          (await page.content()).includes(seed),
          false,
          "closing the dialog removes the key from the page",
        );

        await page
          .getByRole("link", { name: "Open service", exact: true })
          .click();
        await page
          .getByRole("heading", { name: displayName, exact: true })
          .waitFor({ state: "visible", timeout: 30_000 });
        const deploymentId = codecs.ulid.decode(
          new URL(page.url()).pathname.split("/").at(-1),
        );
        const stored = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId,
        });
        assert.equal(
          stored.deployment.participantId,
          participants.EchoProvider.participant.identity,
        );

        // The key the browser created is the instance's identity: a service
        // started with it connects as that instance.
        const connecting = services.add(
          TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.EchoProvider.participant,
            seed,
            signal: services.signal,
          }).orThrow(),
        );
        await connecting;
        const { items } = await runtime.callAdminRpc(
          "authServiceInstancesList",
          { deploymentId },
        );
        assert.deepEqual(
          items.map((instance) => instance.identityPublicKey),
          [
            Buffer.from(
              nacl.sign.keyPair.fromSeed(Buffer.from(seed, "base64url"))
                .publicKey,
            ).toString("base64url"),
          ],
        );
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});

const GAUGES_MANIFEST = `[package]
name = "gauges"
version = "1.0.0"

[sources]
gauges = "gauges.trellis"

[dependencies]
units = { package = "units", workspace = "ghcr.io/example", version = "^2.1.0" }
`;
const GAUGES = `import { Measure } from units;
api gauges@v1 {
  title "Gauges";
  description "Gauge readings.";
  rpc Read { input Measure; output Measure; }
  capabilities {
    capability gauges.read { title "Read gauges"; description "Read gauge values."; grants "Sees gauge values."; allows { rpc Read; } }
  }
}
service Gauge { implements gauges; }
`;
const UNITS_MANIFEST = `[package]
name = "units"
workspace = "ghcr.io/example"
version = "2.1.3"

[sources]
units = "units.trellis"
`;
const UNITS = `model Measure { value: string; }
api units@v1 {
  title "Units";
  description "Units of measure.";
  rpc Convert { input Measure; output Measure; }
  capabilities {
    capability units.convert { title "Convert"; description "Convert units."; grants "Converts units."; allows { rpc Convert; } }
  }
}
`;

const METERS_MANIFEST = `[package]
name = "meters"
version = "1.0.0"

[sources]
meters = "meters.trellis"

[dependencies]
units = { package = "units", workspace = "ghcr.io/example", version = "^2.1.0" }
scales = { package = "scales", workspace = "ghcr.io/example", version = "^1.0.0" }
`;
const METERS = `import { Measure } from units;
import { Scale } from scales;
model Reading { measure: Measure; scale: Scale; }
api meters@v1 {
  title "Meters";
  description "Meter readings.";
  rpc Read { input Reading; output Reading; }
  capabilities {
    capability meters.read { title "Read meters"; description "Read meter values."; grants "Sees meter values."; allows { rpc Read; } }
  }
}
service Meter { implements meters; }
`;
const SCALES_MANIFEST = `[package]
name = "scales"
workspace = "ghcr.io/example"
version = "1.0.2"

[sources]
scales = "scales.trellis"
`;
const SCALES = `model Scale { name: string; }
api scales@v1 {
  title "Scales";
  description "Scales.";
  rpc Name { input Scale; output Scale; }
  capabilities {
    capability scales.name { title "Name"; description "Name scales."; grants "Names scales."; allows { rpc Name; } }
  }
}
`;

test("B01d a second dependency chosen while the first is still resolving keeps both", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const wired = await wireNatsGate(page);
      try {
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const file = (name: string, text: string) => ({
          name,
          mimeType: "text/plain",
          buffer: Buffer.from(text),
        });
        await page
          .getByLabel("Name", { exact: true })
          .fill(`ct-b01d-${ulid()}`);
        await page
          .getByLabel("Service IDL (optional)")
          .setInputFiles([
            file("trellis.toml", METERS_MANIFEST),
            file("meters.trellis", METERS),
          ]);
        const missing = page.getByRole("region", {
          name: "Packages Trellis doesn't have",
        });
        await missing.getByText("scales", { exact: true }).waitFor();
        // Auth's answer for the first package is held while the second is
        // chosen.
        const held = wired.gate.armNextReply(
          authApi.actions["rpc:Packages.Resolve"].subject,
        );
        await missing
          .getByLabel("Files for units", { exact: true })
          .setInputFiles([
            file("trellis.toml", UNITS_MANIFEST),
            file("units.trellis", UNITS),
          ]);
        await held;
        await missing
          .getByLabel("Files for scales", { exact: true })
          .setInputFiles([
            file("trellis.toml", SCALES_MANIFEST),
            file("scales.trellis", SCALES),
          ]);
        wired.releaseToPage();
        await page.getByText("Adds meters.Meter.").waitFor();
        await missing.getByLabel("Files used for units").waitFor();
        await missing.getByLabel("Files used for scales").waitFor();
        assert.equal(
          await page.getByText("Adds meters.Meter.").count(),
          1,
          "the held answer for one package undid the later one",
        );
      } finally {
        wired.gate.dispose();
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B01e an invalid package chosen first is refused without costing the valid one chosen after it", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const wired = await wireNatsGate(page);
      try {
        await openConsoleAsRuntimeAdmin(page, runtime);
        await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const file = (name: string, text: string) => ({
          name,
          mimeType: "text/plain",
          buffer: Buffer.from(text),
        });
        await page
          .getByLabel("Name", { exact: true })
          .fill(`ct-b01e-${ulid()}`);
        await page
          .getByLabel("Service IDL (optional)")
          .setInputFiles([
            file("trellis.toml", METERS_MANIFEST),
            file("meters.trellis", METERS),
          ]);
        const missing = page.getByRole("region", {
          name: "Packages Trellis doesn't have",
        });
        await missing.getByText("scales", { exact: true }).waitFor();
        // The units package doesn't compile; Auth's answer to it is held
        // while a valid scales package is chosen.
        const held = wired.gate.armNextReply(
          authApi.actions["rpc:Packages.Resolve"].subject,
        );
        await missing
          .getByLabel("Files for units", { exact: true })
          .setInputFiles([
            file("trellis.toml", UNITS_MANIFEST),
            file("units.trellis", "model Measure { value: }"),
          ]);
        await held;
        await missing
          .getByLabel("Files for scales", { exact: true })
          .setInputFiles([
            file("trellis.toml", SCALES_MANIFEST),
            file("scales.trellis", SCALES),
          ]);
        wired.releaseToPage();
        // Scales is accepted; only units is refused, and it stays missing.
        await missing.getByLabel("Files used for scales").waitFor();
        assert.equal(
          await missing.getByLabel("Files used for units").count(),
          0,
          "the package that failed to compile was used",
        );
        await missing
          .getByLabel("Files for units", { exact: true })
          .setInputFiles([
            file("trellis.toml", UNITS_MANIFEST),
            file("units.trellis", UNITS),
          ]);
        await page.getByText("Adds meters.Meter.").waitFor();
        await missing.getByLabel("Files used for scales").waitFor();
      } finally {
        wired.gate.dispose();
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B01f a package uploaded under another package's row is refused on that row and not kept", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const file = (name: string, text: string) => ({
        name,
        mimeType: "text/plain",
        buffer: Buffer.from(text),
      });
      await page.getByLabel("Name", { exact: true }).fill(`ct-b01f-${ulid()}`);
      await page
        .getByLabel("Service IDL (optional)")
        .setInputFiles([
          file("trellis.toml", METERS_MANIFEST),
          file("meters.trellis", METERS),
        ]);
      const missing = page.getByRole("region", {
        name: "Packages Trellis doesn't have",
      });
      await missing.getByText("scales", { exact: true }).waitFor();
      // A valid units package uploaded under scales' row is refused on that
      // row and not kept, so it never rides along in later requests.
      await missing
        .getByLabel("Files for scales", { exact: true })
        .setInputFiles([
          file("trellis.toml", UNITS_MANIFEST),
          file("units.trellis", UNITS),
        ]);
      await missing
        .getByRole("alert")
        .getByText("These files don't provide a scales that matches.")
        .waitFor();
      assert.equal(
        await missing.getByLabel("Files used for scales").count(),
        0,
      );
      // Each package under its own row is then used, and the service added.
      await missing
        .getByLabel("Files for units", { exact: true })
        .setInputFiles([
          file("trellis.toml", UNITS_MANIFEST),
          file("units.trellis", UNITS),
        ]);
      await missing.getByLabel("Files used for units").waitFor();
      await missing
        .getByLabel("Files for scales", { exact: true })
        .setInputFiles([
          file("trellis.toml", SCALES_MANIFEST),
          file("scales.trellis", SCALES),
        ]);
      await page.getByText("Adds meters.Meter.").waitFor();
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B01g files for a dependency still being read when the IDL changes never reach the new draft", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      // Reading units.trellis waits until the test lets it finish, so the IDL
      // can change in between.
      await page.addInitScript(() => {
        const read = File.prototype.text;
        const state = window as unknown as {
          holdUnits?: boolean;
          releaseUnits?: () => void;
          unitsRead?: boolean;
        };
        File.prototype.text = async function (this: File) {
          if (this.name === "units.trellis" && state.holdUnits) {
            await new Promise<void>((resolve) => {
              state.releaseUnits = resolve;
            });
            const text = await read.call(this);
            state.unitsRead = true;
            return text;
          }
          return await read.call(this);
        };
      });
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      const file = (name: string, text: string) => ({
        name,
        mimeType: "text/plain",
        buffer: Buffer.from(text),
      });
      await page.getByLabel("Name", { exact: true }).fill(`ct-b01g-${ulid()}`);
      const idl = page.getByLabel("Service IDL (optional)");
      await idl.setInputFiles([
        file("trellis.toml", METERS_MANIFEST),
        file("meters.trellis", METERS),
      ]);
      const missing = page.getByRole("region", {
        name: "Packages Trellis doesn't have",
      });
      await missing.getByText("scales", { exact: true }).waitFor();
      await page.evaluate(() => {
        (window as unknown as { holdUnits: boolean }).holdUnits = true;
      });
      await missing
        .getByLabel("Files for units", { exact: true })
        .setInputFiles([
          file("trellis.toml", UNITS_MANIFEST),
          file("units.trellis", UNITS),
        ]);
      await page.waitForFunction(
        () =>
          typeof (window as unknown as { releaseUnits?: () => void })
            .releaseUnits === "function",
      );
      // The IDL changes to one that needs only units, then the held read ends.
      await idl.setInputFiles([
        file("trellis.toml", GAUGES_MANIFEST),
        file("gauges.trellis", GAUGES),
      ]);
      await missing.getByText("^2.1.0", { exact: true }).waitFor();
      await page.evaluate(() => {
        (window as unknown as { releaseUnits: () => void }).releaseUnits();
      });
      await page.waitForFunction(
        () => (window as unknown as { unitsRead?: boolean }).unitsRead === true,
      );
      // Had the old selection been kept, the new draft would resolve within
      // one round trip; it must stay waiting for units.
      for (let check = 0; check < 10; check++) {
        assert.equal(
          await page.getByText("Adds gauges.Gauge.").count(),
          0,
          "files chosen for the earlier IDL reached the new draft",
        );
        await page.waitForTimeout(150);
      }
      assert.equal(await missing.getByLabel("Files used for units").count(), 0);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B01c a dependency Trellis doesn't have is listed, and adding its files lets the service be added", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);

      const file = (name: string, text: string) => ({
        name,
        mimeType: "text/plain",
        buffer: Buffer.from(text),
      });
      const displayName = `ct-b01c-${ulid()}`;
      await page.getByLabel("Name", { exact: true }).fill(displayName);
      await page
        .getByLabel("Service IDL (optional)")
        .setInputFiles([
          file("trellis.toml", GAUGES_MANIFEST),
          file("gauges.trellis", GAUGES),
        ]);
      const missing = page.getByRole("region", {
        name: "Packages Trellis doesn't have",
      });
      await missing.getByText("units", { exact: true }).waitFor();
      await missing.getByText("^2.1.0", { exact: true }).waitFor();
      const add = page.getByRole("button", {
        name: "Add service",
        exact: true,
      });
      assert.equal(
        await add.isDisabled(),
        true,
        "a service with a missing dependency cannot be added",
      );

      // A selection without its trellis.toml is refused on the package's own
      // row; the row stays so the files can be chosen again.
      const unitsFiles = missing.getByLabel("Files for units", { exact: true });
      await unitsFiles.setInputFiles([file("units.trellis", UNITS)]);
      await missing
        .getByText("Add the trellis.toml for units with its files.")
        .waitFor();
      await unitsFiles.setInputFiles([
        file("trellis.toml", UNITS_MANIFEST),
        file("units.trellis", UNITS),
      ]);
      // The service's own IDL was never chosen again.
      await page.getByText("Adds gauges.Gauge.").waitFor();
      await missing.getByLabel("Files used for units").waitFor();
      assert.equal(
        await missing
          .getByText("Add the trellis.toml for units with its files.")
          .count(),
        0,
      );
      await add.click();
      const dialog = page.getByRole("dialog", {
        name: "Save the service's key",
      });
      await dialog
        .getByRole("button", { name: "I've saved it", exact: true })
        .click();
      const deploymentId = codecs.ulid.decode(
        await page.getByTestId("created-id").textContent(),
      );
      const stored = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId,
      });
      assert.equal(stored.deployment.participantId, "localhost/gauges.Gauge");
      assert.equal(stored.deployment.title, displayName);
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B01b adding a service without its IDL and with its own public key waits for that service to introduce itself", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(`${runtime.consoleUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);

        const displayName = `ct-b01b-${ulid()}`;
        const seed = randomBytes(32);
        await page.getByLabel("Name", { exact: true }).fill(displayName);
        await page.getByLabel("I already have a key").check();
        const publicKey = page.getByLabel("Public key", { exact: true });
        await publicKey.fill("not-a-key");
        await page.getByText("A public key is 43 letters").waitFor();
        assert.equal(
          await page
            .getByRole("button", { name: "Add service", exact: true })
            .isDisabled(),
          true,
          "a malformed public key cannot be submitted",
        );
        await publicKey.fill(
          Buffer.from(nacl.sign.keyPair.fromSeed(seed).publicKey).toString(
            "base64url",
          ),
        );
        await page
          .getByRole("button", { name: "Add service", exact: true })
          .click();
        await page
          .getByRole("link", { name: "Open service", exact: true })
          .click();
        await page
          .getByRole("heading", { name: displayName, exact: true })
          .waitFor({ state: "visible", timeout: 30_000 });
        await page.getByText("Waiting for its service").waitFor();
        const claimId = new URL(page.url()).pathname.split("/").at(-1);

        // The service holding the matching seed introduces itself to this
        // claim and waits for approval.
        services.add(
          TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: participants.EchoProvider.participant,
            seed: seed.toString("base64url"),
            signal: services.signal,
          }).orThrow(),
        );
        await runtime.waitFor(async () => {
          const { items } = await runtime.callAdminRpc(
            "authServiceUpdatesList",
            {},
          );
          return items.find(
            (update) =>
              update.deploymentId === claimId && update.state === "pending",
          );
        });
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});

test("B02 installed service resources render from real evidence", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedServiceWithResources(runtime, "b02");
    try {
      const detail = await runtime.callAdminRpc("authResourcesList", {
        ownerId: seeded.deploymentId,
      });
      const resourceCount = detail.items.length;
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(`${runtime.consoleUrl}/console/admin/resources`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        const resourceRows = page
          .getByRole("region", { name: "All storage" })
          .locator("tbody tr")
          .filter({ hasText: seeded.displayName });
        await resourceRows.first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assert.deepEqual(
          await resourceRows.count(),
          resourceCount,
          "expected one rendered row per materialized resource",
        );
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      // The live provider must stop before runtime teardown, or its socket
      // surfaces an uncaught transport reset in a later test.
      await seeded.stop();
    }
  }, browserRuntimeOptions());
});

test("B03 same-document service navigation replaces A with B", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedServiceWithResources(runtime, "b03-a");
    try {
      const b = await seedServiceWithoutResources(runtime, "b03-b");
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors = await captureBrowserErrors(page);
        await openConsoleAsRuntimeAdmin(page, runtime);

        await page.goto(
          `${runtime.consoleUrl}/console/admin/services/${a.deploymentId}`,
          { waitUntil: "domcontentloaded" },
        );
        await page
          .getByRole("heading", { name: a.displayName, exact: true })
          .waitFor({
            state: "visible",
          });

        await page.evaluate((deploymentId) => {
          document
            .querySelector('[data-testid="console-ready"]')
            ?.setAttribute("id", "same-console-document");
          const link = document.createElement("a");
          link.href = `/console/admin/services/${encodeURIComponent(
            deploymentId,
          )}`;
          document.body.append(link);
          link.click();
          link.remove();
        }, b.deploymentId);
        await page.waitForURL((url) =>
          url.pathname.endsWith(`/${b.deploymentId}`),
        );
        await page.getByText(b.displayName).first().waitFor({
          state: "visible",
          timeout: 30_000,
        });
        assert.deepEqual(
          await page.getByText(a.displayName).count(),
          0,
          "service A's content must not remain after navigating to B",
        );
        assert.deepEqual(
          await page.locator("#same-console-document").count(),
          1,
          "the document and authority shell were reused",
        );
        await assertNoBrowserErrors(errors);
      } finally {
        await context.close();
      }
    } finally {
      await a.stop();
    }
  }, browserRuntimeOptions());
});

test("B04 a missing service ID renders unavailable, not a fallback", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.consoleUrl}/console/admin/services/${ulid()}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Not found", { exact: false }).first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("button", { name: "Refresh", exact: true }).waitFor({
        state: "visible",
      });
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B05 service table pages and resets on filter change", async () => {
  await withTrellisRuntime(async (runtime) => {
    for (let index = 0; index < 52; index += 1) {
      await seedServiceWithoutResources(runtime, `b05-${index}`);
    }
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(`${runtime.consoleUrl}/console/admin/services`, {
        waitUntil: "domcontentloaded",
      });
      await waitForConsoleShell(page);
      await page
        .getByRole("heading", { name: "Services", exact: true })
        .waitFor({ state: "visible", timeout: 30_000 });
      await page.getByText("on this page").first().waitFor({
        state: "visible",
      });
      const rows = page.locator(
        'main tbody a[href^="/console/admin/services/"]',
      );
      const firstPageId = await rows.first().getAttribute("href");
      await page
        .getByRole("button", { name: "Next page", exact: true })
        .click();
      await page
        .getByRole("button", { name: "Previous page", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.waitForFunction(
        (first) =>
          document
            .querySelector('main tbody a[href^="/console/admin/services/"]')
            ?.getAttribute("href") !== first,
        firstPageId,
      );
      assert.deepEqual(
        (await rows.first().getAttribute("href")) !== firstPageId,
        true,
      );

      // Filtering hides unmatched rows; clearing it must return to the first page.
      await page.getByRole("button", { name: "Disabled", exact: true }).click();
      await runtime.waitFor(async () => (await rows.count()) === 0);
      await page.getByRole("button", { name: "All", exact: true }).click();
      await page.waitForFunction(
        (first) =>
          document
            .querySelector('main tbody a[href^="/console/admin/services/"]')
            ?.getAttribute("href") === first,
        firstPageId,
      );
      assert.deepEqual(
        await page
          .getByRole("button", { name: "Previous page", exact: true })
          .isDisabled(),
        true,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B06 device creation persists the authored participant and returns its allocated identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.contracts.install({
      contract: participants.Device.participant,
    });
    await runtime.contracts.install({
      contract: participants.ChildProvider.participant,
    });
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/profiles/new`,
        {
          waitUntil: "domcontentloaded",
        },
      );
      await waitForConsoleShell(page);

      const displayName = `ct-b06-${ulid()}`;
      await page.getByLabel("Name", { exact: true }).fill(displayName);
      await page.getByLabel("Device IDL").setInputFiles(fixtureIdl);
      await page
        .getByLabel("Which device")
        .selectOption(participants.Device.participant.identity);
      await page
        .getByRole("button", { name: "Add device type", exact: true })
        .click();
      try {
        await page
          .getByText(`${displayName} was added.`)
          .waitFor({ state: "visible", timeout: 30_000 });
      } catch (cause) {
        throw new Error(
          `Device creation did not resolve: ${await page.locator("main").innerText()}`,
          { cause },
        );
      }
      const createdId = codecs.ulid.decode(
        await page.getByTestId("created-id").textContent(),
      );
      const listed = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: createdId,
      });
      assert.equal(listed.deployment.title, displayName);
      assert.equal(
        listed.deployment.participantId,
        participants.Device.participant.identity,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B07 provisioning deep link selects exactly the requested deployment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const target = await seedDeviceDeployment(runtime, "b07-target");
    const other = await seedDeviceDeployment(runtime, "b07-other");
    const targetId = target.deploymentId;
    const otherId = other.deploymentId;

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/instances/provision?deploymentId=${targetId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      const field = page.getByLabel("Deployment");
      await field.waitFor({ state: "visible", timeout: 30_000 });
      assert.deepEqual(await field.inputValue(), targetId);
      assert.deepEqual((await field.inputValue()) === otherId, false);
      // A read-only field proves the deep link cannot drift to another target.
      assert.deepEqual((await field.getAttribute("readonly")) !== null, true);

      const missingId = ulid();
      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/instances/provision?deploymentId=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await page
        .getByText(`No deployment matches '${missingId}'`)
        .first()
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      assert.equal(
        await page
          .getByRole("button", { name: "Provision", exact: true })
          .isDisabled(),
        true,
        "an unavailable deployment cannot be provisioned",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("N05 device disable after query navigation affects only the new target", async () => {
  await withTrellisRuntime(async (runtime) => {
    const a = await seedDeviceDeployment(runtime, "n05-a");
    const b = await seedDeviceDeployment(runtime, "n05-b");
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);
      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/profiles/disable?deployment=${a.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByLabel("Deployment").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      await page.getByRole("button", { name: "Disable deployment" }).click();
      await page.locator("dialog[open]").waitFor({ state: "visible" });
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/devices/profiles/disable?deployment=${encodeURIComponent(
          id,
        )}`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, b.deploymentId);
      await page.waitForURL(
        (url) => url.searchParams.get("deployment") === b.deploymentId,
      );
      await page.locator("dialog[open]").waitFor({ state: "hidden" });
      for (const target of [a, b]) {
        const unchanged = await runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: target.deploymentId,
        });
        assert.equal(
          unchanged.deployment.state,
          "enabled",
          "stale confirmation must disable neither deployment",
        );
      }
      await page.getByLabel("Deployment").waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.getByLabel("Deployment").inputValue(),
        b.deploymentId,
      );
      await page.getByRole("button", { name: "Disable deployment" }).click();
      const dialog = page.locator("dialog[open]");
      await dialog.waitFor({ state: "visible" });
      await dialog.locator("input").fill(b.deploymentId);
      await dialog.getByRole("button", { name: "Disable deployment" }).click();
      await page
        .getByText(`Device deployment ${b.deploymentId} is disabled.`)
        .waitFor({ state: "visible", timeout: 30_000 });
      const [unchanged, disabled] = await Promise.all([
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: a.deploymentId,
        }),
        runtime.callAdminRpc("authDeploymentsGet", {
          deploymentId: b.deploymentId,
        }),
      ]);
      assert.deepEqual(unchanged.deployment.state, "enabled");
      assert.deepEqual(disabled.deployment.state, "disabled");
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B08 provisioning receipt shows the returned identity", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedDeviceDeployment(runtime, "b08-device");
    const deploymentId = seeded.deploymentId;
    const other = await seedDeviceDeployment(runtime, "b08-next");

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = await captureBrowserErrors(page);
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/instances/provision?deploymentId=${deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .waitFor({
          state: "visible",
          timeout: 30_000,
        });
      await page.getByLabel("Instance title").fill("B08 first instance");
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .click();

      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      const receiptText = await page
        .locator('[data-testid="console-page-ready"], main')
        .first()
        .innerText();
      assert.deepEqual(receiptText.includes(deploymentId), true);
      assert.deepEqual(
        (await page.getByText("Store this secret now.").count()) > 0,
        true,
      );
      const firstInstanceId = await page
        .locator("dd.trellis-identifier")
        .nth(1)
        .innerText();
      await page.evaluate((id) => {
        const link = document.createElement("a");
        link.href = `/console/admin/devices/instances/provision?deploymentId=${encodeURIComponent(
          id,
        )}`;
        (document.querySelector("main") ?? document.body).append(link);
        link.click();
      }, other.deploymentId);
      await page.waitForURL(
        (url) => url.searchParams.get("deploymentId") === other.deploymentId,
      );
      await page
        .getByText(
          `This one-time receipt belongs to deployment ${deploymentId}, not the currently requested deployment ${other.deploymentId}.`,
          { exact: false },
        )
        .waitFor({ state: "visible", timeout: 30_000 });

      // Starting another operation clears the previous one-time secret.
      await page
        .getByRole("button", { name: "Provision another instance" })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "hidden",
        timeout: 10_000,
      });
      await page.getByLabel("Instance title").fill("B08 second instance");
      await page
        .getByRole("button", { name: "Provision", exact: true })
        .click();
      await page.getByText("Provisioning receipt").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assert.deepEqual(
        await page.locator("dd.trellis-identifier").first().innerText(),
        other.deploymentId,
      );
      assert.deepEqual(
        (await page.locator("dd.trellis-identifier").nth(1).innerText()) !==
          firstInstanceId,
        true,
      );
      await assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B09 missing device action targets never default to another row", async () => {
  await withTrellisRuntime(async (runtime) => {
    const seeded = await seedDeviceDeployment(runtime, "b09-device");
    const missingId = ulid();
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/profiles/disable?deployment=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active deployment").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      await page.goto(
        `${runtime.consoleUrl}/console/admin/devices/instances/disable?instance=${missingId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("is not an active instance").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });

      const unchanged = await runtime.callAdminRpc("authDeploymentsGet", {
        deploymentId: seeded.deploymentId,
      });
      assert.equal(
        unchanged.deployment.state,
        "enabled",
        "unavailable targets must not mutate the existing deployment",
      );
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});

test("B12 device back links stay inside the configured console path", async () => {
  await withTrellisRuntime(async (runtime) => {
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsoleAsRuntimeAdmin(page, runtime);

      for (const route of [
        "/console/admin/devices/profiles/new",
        "/console/admin/devices/instances/provision",
      ]) {
        await page.goto(`${runtime.consoleUrl}${route}`, {
          waitUntil: "domcontentloaded",
        });
        await waitForConsoleShell(page);
        await page
          .getByRole("link", { name: /Back to (devices|deployments)/ })
          .first()
          .click();
        await page.waitForURL(
          (url) => url.pathname === "/console/admin/devices",
        );
        await waitForConsoleShell(page);
      }
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
