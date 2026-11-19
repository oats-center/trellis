import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { promisify } from "node:util";
import { test } from "node:test";
import { chromium } from "playwright";
import nacl from "tweetnacl";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { codecs } from "../packages/trellis/generated.ts";
import { API as authApi } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
  loginCliAsAdministrator,
  startedServices,
} from "./browser_test_support.ts";
import { wireNatsGate } from "./console_test_support.ts";

const participant = participants.ConfiguredService.participant;

test("an administrator sets a waiting service's settings from the link it printed", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const key = await runtime.services.createInstance({
        name: "configured",
        deployment: "configured",
        contract: participant,
      });
      // The service has no endpoint or key yet, so it waits and prints a link.
      const connecting = services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant,
          seed: key.seed,
          signal: services.signal,
        }).orThrow(),
      );
      const target = { deploymentId: codecs.ulid.decode(key.deploymentId) };
      await runtime.waitFor(async () => {
        const view = await runtime.callAdminRpc("authConfigGet", { target });
        return view.instances[0]?.connectedAt;
      });

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${key.deploymentId}#settings`,
          { waitUntil: "domcontentloaded" },
        );
        await completeConsoleEntry(page, BROWSER_ADMIN);
        const settings = page.getByRole("region", { name: "Settings" });
        // The settings its instances wait for are the ones marked required.
        await settings.getByLabel("Endpoint", { exact: true }).waitFor();
        assert.equal(
          await settings.getByText("Required", { exact: true }).count(),
          2,
        );
        await settings
          .getByLabel("Endpoint", { exact: true })
          .fill("https://one.example");
        await settings.getByLabel("API key", { exact: true }).fill("k1");
        // A secret is typed hidden, and only what was typed can be shown.
        const typedKey = settings.getByLabel("API key", { exact: true });
        assert.equal(await typedKey.getAttribute("type"), "password");
        await settings
          .getByRole("button", { name: "Show API key", exact: true })
          .click();
        assert.equal(await typedKey.getAttribute("type"), "text");
        assert.equal(await typedKey.inputValue(), "k1");
        // A nullable secret takes plain text, not JSON.
        await settings.getByLabel("Signing key", { exact: true }).fill("s1");
        await settings
          .getByRole("button", { name: "Save settings", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        // The secret is never shown back; its field only says it is set.
        const apiKey = settings.getByLabel("API key", { exact: true });
        assert.equal(await apiKey.inputValue(), "");
        assert.equal(
          await apiKey.getAttribute("placeholder"),
          "Set — type to replace",
        );
        // Showing lasted only as long as the saved draft: a new key starts
        // hidden. Emptying it again leaves the saved key alone.
        await apiKey.fill("k2");
        assert.equal(await apiKey.getAttribute("type"), "password");
        await apiKey.fill("");
        // Trellis restarts nothing, so a setting that applies after a restart
        // asks first; cancelling saves nothing and keeps the edit.
        const port = settings.getByLabel("Port", { exact: true });
        await port.fill("9090");
        const saveSettings = settings.getByRole("button", {
          name: "Save settings",
          exact: true,
        });
        await saveSettings.click();
        const restart = page.getByRole("dialog", { name: "Restart to apply" });
        await restart.getByText(/restart its instances yourself/).waitFor();
        await restart
          .getByRole("button", { name: "Cancel", exact: true })
          .first()
          .click();
        await restart.waitFor({ state: "hidden" });
        assert.equal(await port.inputValue(), "9090");
        assert.equal(await saveSettings.isEnabled(), true);
        await saveSettings.click();
        await restart
          .getByRole("button", { name: "Save", exact: true })
          .click();
        await settings.getByText("Settings saved.").waitFor();
        assert.equal(await saveSettings.isEnabled(), false);
        assert.equal(await port.inputValue(), "9090");
      } finally {
        await admin.close();
      }

      const service = await connecting;
      assert.equal(service.config.current.endpoint, "https://one.example");
      assert.equal(service.config.current.apiKey, "k1");
      assert.equal(service.config.current.signingKey, "s1");
    }, browserRuntimeOptions());
  } finally {
    // A failed step leaves the service waiting for its settings.
    await services.stop();
  }
});

test("an administrator overrides the only instance's settings, null included, without changing the service's", async () => {
  const services: { stop(): Promise<void> }[] = [];
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const key = await runtime.services.createInstance({
        name: "single",
        deployment: "single",
        contract: participant,
        config: {
          values: { endpoint: "https://one.example", region: "east" },
          secrets: { apiKey: "k1", signingKey: "s1" },
        },
      });
      const deploymentId = codecs.ulid.decode(key.deploymentId);
      const before = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId },
      });
      assert.equal(before.instances.length, 1);
      const instanceId = before.instances[0]!.instanceId;
      const valueOf = (view: typeof before, name: string) => {
        const field = view.fields.find((item) => item.name === name);
        return field && { value: field.value, source: field.source };
      };

      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        const wired = await wireNatsGate(page);
        try {
          await page.goto(
            `${runtime.trellisUrl}/console/admin/services/${key.deploymentId}#settings`,
            { waitUntil: "domcontentloaded" },
          );
          await completeConsoleEntry(page, BROWSER_ADMIN);
          const settings = page.getByRole("region", { name: "Settings" });
          const region = settings.getByLabel("Region", { exact: true });
          await runtime.waitFor(
            async () => (await region.inputValue()) === "east",
          );
          const save = settings.getByRole("button", {
            name: "Save settings",
            exact: true,
          });

          // A draft typed for the whole service, then the only instance is
          // chosen while its settings are still on the way.
          await region.fill("everywhere");
          const apiKey = settings.getByLabel("API key", { exact: true });
          await apiKey.fill("kx");
          await settings
            .getByRole("button", { name: "Show API key", exact: true })
            .click();
          assert.equal(await apiKey.getAttribute("type"), "text");
          const held = wired.gate.armNextReply(
            authApi.actions["rpc:Config.Get"].subject,
          );
          await settings
            .getByRole("combobox")
            .selectOption({ value: instanceId });
          await held;
          // Saving now would write the service's draft for every instance,
          // so nothing can be edited or saved until the instance loads.
          assert.equal(
            await save.isDisabled(),
            true,
            "Save was offered before the instance's settings loaded",
          );
          assert.equal(await region.isDisabled(), true);
          await save.click({ timeout: 1_000 }).catch(() => {});
          wired.releaseToPage();
          await settings.getByText("Same as every instance").first().waitFor();
          await runtime.waitFor(() => region.isEnabled());
          assert.equal(
            await region.inputValue(),
            "east",
            "the service's draft was saved or carried over to the instance",
          );
          assert.equal(await save.isDisabled(), true);

          // Null is stored for the instance, unlike Clear, which inherits.
          // Setting one field to null keeps the other unsaved edits, a typed
          // secret included, pending for Save.
          const endpoint = settings.getByLabel("Endpoint", { exact: true });
          await region.fill("west");
          await endpoint.fill("https://two.example");
          await apiKey.fill("k2");
          // What was shown for the service stays with it: the instance's new
          // key is typed hidden.
          assert.equal(await apiKey.getAttribute("type"), "password");
          await settings
            .getByRole("button", { name: "Clear Region", exact: true })
            .click();
          await settings.getByText("Region cleared.").waitFor();
          assert.equal(await region.inputValue(), "");
          assert.equal(await region.getAttribute("placeholder"), "No value");
          await settings
            .getByRole("button", {
              name: "Clear Signing key",
              exact: true,
            })
            .click();
          await settings.getByText("Signing key cleared.").waitFor();
          assert.equal(await endpoint.inputValue(), "https://two.example");
          assert.equal(await apiKey.inputValue(), "k2");
          await save.click();
          await settings.getByText("Settings saved.").waitFor();
          assert.equal(await region.inputValue(), "");
          assert.equal(await region.getAttribute("placeholder"), "No value");
        } finally {
          wired.gate.dispose();
        }
      } finally {
        await admin.close();
      }

      const after = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId },
      });
      assert.equal(after.revision, before.revision);
      assert.deepEqual(valueOf(after, "region"), {
        value: "east",
        source: "deployment",
      });
      assert.deepEqual(valueOf(after, "endpoint"), {
        value: "https://one.example",
        source: "deployment",
      });
      const instance = await runtime.callAdminRpc("authConfigGet", {
        target: { deploymentId, instanceId },
      });
      assert.deepEqual(valueOf(instance, "region"), {
        value: null,
        source: "instance",
      });
      assert.deepEqual(valueOf(instance, "endpoint"), {
        value: "https://two.example",
        source: "instance",
      });

      // The instance itself reads its own overrides, the null secret too.
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant,
        seed: key.seed,
      }).orThrow();
      services.push(service);
      assert.equal(service.config.current.endpoint, "https://two.example");
      assert.equal(service.config.current.region, null);
      assert.equal(service.config.current.signingKey, null);
      assert.equal(service.config.current.apiKey, "k2");
    }, browserRuntimeOptions());
  } finally {
    for (const service of services) await service.stop();
  }
});

test("a service added without its IDL starts with the settings entered when it was added", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const seed = randomBytes(32);
      const publicKey = Buffer.from(
        nacl.sign.keyPair.fromSeed(seed).publicKey,
      ).toString("base64url");
      let claimId: string;
      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        await page.goto(`${runtime.trellisUrl}/console/admin/services/new`, {
          waitUntil: "domcontentloaded",
        });
        await completeConsoleEntry(page, BROWSER_ADMIN);
        await page.getByLabel("Name", { exact: true }).fill("orders-claimed");
        await page.getByText("I already have a key", { exact: true }).click();
        await page.getByLabel("Public key", { exact: true }).fill(publicKey);
        // Without an IDL, each setting is a name and a value, typed by hand.
        const add = page.getByRole("button", {
          name: "Add a setting",
          exact: true,
        });
        await add.click();
        await page
          .getByLabel("Name of setting 1", { exact: true })
          .fill("endpoint");
        await page
          .getByLabel("Value of setting 1", { exact: true })
          .fill("https://one.example");
        await add.click();
        await page
          .getByLabel("Name of setting 2", { exact: true })
          .fill("apiKey");
        await page.getByLabel("Setting 2 is a secret", { exact: true }).check();
        const secret = page.getByLabel("Value of setting 2", { exact: true });
        assert.equal(await secret.getAttribute("type"), "password");
        await secret.fill("k1");
        await page
          .getByRole("button", { name: "Add service", exact: true })
          .click();
        await page.getByText("orders-claimed was added.").waitFor();
        claimId = (await page.getByTestId("created-id").textContent()) ?? "";
        assert.notEqual(claimId, "");
      } finally {
        await admin.close();
      }

      // The CLI sets more by the service's name before its IDL is known:
      // without a declaration, a value is JSON when it parses and text
      // otherwise.
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const { run } = await loginCliAsAdministrator(
          runtime,
          await browser.newPage(),
        );
        const set = await run(
          "svc",
          "orders-claimed",
          "config",
          "set",
          "region=east",
          "legacyMode=true",
        );
        assert.equal(set.declared, false);
        assert.deepEqual(
          set.unused
            .filter((item: { secret: boolean }) => !item.secret)
            .map((item: { name: string; value: unknown }) => [
              item.name,
              item.value,
            ])
            .sort(),
          [
            ["endpoint", "https://one.example"],
            ["legacyMode", true],
            ["region", "east"],
          ],
        );
      } finally {
        await browser.close();
      }

      // The service connects with its IDL and is approved.
      const connecting = services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant,
          seed: seed.toString("base64url"),
          signal: services.signal,
        }).orThrow(),
      );
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find(
          (update) =>
            update.deploymentId === claimId && update.state === "pending",
        );
      });
      const { update } = await runtime.callAdminRpc(
        "authServiceUpdatesDecide",
        {
          updateId: pending.updateId,
          decision: "approved",
          expectedRevision: pending.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        },
      );
      const service = await connecting;
      assert.equal(service.config.current.endpoint, "https://one.example");
      assert.equal(service.config.current.apiKey, "k1");
      assert.equal(service.config.current.region, "east");

      // Its page keeps the setting its build doesn't use, marked as such.
      const viewer = await launchProfile(runtime);
      try {
        const page = await viewer.newPage();
        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${update.deploymentId}#settings`,
          { waitUntil: "domcontentloaded" },
        );
        await completeConsoleEntry(page, BROWSER_ADMIN);
        const settings = page.getByRole("region", { name: "Settings" });
        const unused = settings.getByLabel("Settings this build doesn't use");
        await unused.getByText("Not used by this build").waitFor();
        await unused.getByText("legacyMode", { exact: true }).waitFor();
        assert.equal(
          await settings.getByLabel("Region", { exact: true }).inputValue(),
          "east",
        );
      } finally {
        await viewer.close();
      }
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});

test("a plain value saved under a name the IDL then declares secret is never shown or used, and setting the secret starts the service", async () => {
  const services = startedServices();
  try {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const plain = `plain-${ulid()}`;
      const seed = randomBytes(32);
      const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
        title: "plain-key",
        instanceTitle: "plain-key",
        identityPublicKey: Buffer.from(
          nacl.sign.keyPair.fromSeed(seed).publicKey,
        ).toString("base64url"),
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      // Before Trellis has the IDL, the key is saved as a plain value.
      await runtime.callAdminRpc("authConfigSet", {
        target: { claimId: claim.claimId },
        values: { endpoint: "https://one.example", apiKey: plain },
        idempotencyKey: codecs.ulid.decode(ulid()),
      });

      // The service connects with an IDL that declares apiKey secret.
      const connecting = services.add(
        TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant,
          seed: seed.toString("base64url"),
          signal: services.signal,
        }).orThrow(),
      );
      const pending = await runtime.waitFor(async () => {
        const { items } = await runtime.callAdminRpc(
          "authServiceUpdatesList",
          {},
        );
        return items.find(
          (update) =>
            update.deploymentId === claim.claimId && update.state === "pending",
        );
      });
      const { update } = await runtime.callAdminRpc(
        "authServiceUpdatesDecide",
        {
          updateId: pending.updateId,
          decision: "approved",
          expectedRevision: pending.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        },
      );
      const target = { deploymentId: update.deploymentId };

      // The plain value is reported, never shown, and the service waits.
      const waiting = await runtime.waitFor(async () => {
        const view = await runtime.callAdminRpc("authConfigGet", { target });
        return view.instances[0]?.connectedAt === undefined ? undefined : view;
      });
      assert.deepEqual(waiting.missing, ["apiKey"]);
      const apiKey = waiting.fields.find((field) => field.name === "apiKey");
      assert.equal(apiKey?.storedAsPlain, true);
      assert.match(apiKey?.invalid ?? "", /stored as a plain setting/);
      assert.doesNotMatch(JSON.stringify(waiting), new RegExp(plain));
      const history = await runtime.callAdminRpc("authConfigRevisionsList", {
        target,
      });
      assert.deepEqual(history.items[0]?.hiddenValues, ["apiKey"]);
      assert.doesNotMatch(JSON.stringify(history), new RegExp(plain));
      assert.equal(
        await Promise.race([
          connecting.then(() => "connected"),
          Promise.resolve("waiting"),
        ]),
        "waiting",
      );

      // The Console says what happened and how to fix it.
      const admin = await launchProfile(runtime);
      try {
        const page = await admin.newPage();
        await page.goto(
          `${runtime.trellisUrl}/console/admin/services/${update.deploymentId}#settings`,
          { waitUntil: "domcontentloaded" },
        );
        await completeConsoleEntry(page, BROWSER_ADMIN);
        const settings = page.getByRole("region", { name: "Settings" });
        await settings
          .getByText(/Stored as plain text; set it as a secret/)
          .waitFor();
        await settings
          .getByText(/old\s+plain\s+value\s+stays\s+in\s+earlier\s+versions/)
          .waitFor();
        assert.doesNotMatch(await settings.innerText(), new RegExp(plain));
      } finally {
        await admin.close();
      }

      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      try {
        const { directory, run } = await loginCliAsAdministrator(
          runtime,
          await browser.newPage(),
        );
        const binary = process.env.TRELLIS_TEST_CLI_BIN!;
        const env = { ...process.env, XDG_CONFIG_HOME: directory };
        // The CLI's table names the fix and never prints the value.
        const shown = await promisify(execFile)(
          binary,
          ["svc", "plain-key", "config"],
          { env, timeout: 15_000 },
        );
        const text = shown.stdout + shown.stderr;
        assert.match(text, /stored as plain text/);
        assert.match(text, /config set-secret apiKey/);
        assert.doesNotMatch(text, new RegExp(plain));
        assert.doesNotMatch(
          JSON.stringify(await run("svc", "plain-key", "config", "history")),
          new RegExp(plain),
        );

        // Setting it as a secret, piped as a script would, starts the
        // service with the secret, never the plain value.
        const setSecret = spawn(
          binary,
          [
            "--format",
            "json",
            "svc",
            "plain-key",
            "config",
            "set-secret",
            "apiKey",
          ],
          { env, stdio: ["pipe", "pipe", "pipe"] },
        );
        setSecret.stdin.end("sealed-key\n");
        const [code] = await once(setSecret, "exit");
        assert.equal(code, 0);
      } finally {
        await browser.close();
      }
      const service = await connecting;
      assert.equal(service.config.current.apiKey, "sealed-key");
      const fixed = await runtime.callAdminRpc("authConfigGet", { target });
      assert.equal(
        fixed.fields.find((field) => field.name === "apiKey")?.storedAsPlain,
        false,
      );
    }, browserRuntimeOptions());
  } finally {
    await services.stop();
  }
});
