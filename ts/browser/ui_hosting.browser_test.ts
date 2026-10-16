import assert from "node:assert/strict";
import { test } from "node:test";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  signInIfPrompted,
  waitForConsoleReady,
} from "./browser_test_support.ts";

test(
  "embedded Console signs in through the isolated OAuth Portal and returns to the application origin",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const context = await launchProfile(runtime);
      try {
        const page = await context.newPage();
        const errors: Error[] = [];
        page.on("pageerror", (error) => errors.push(error));
        await page.goto(`${runtime.publicOrigin}/console`, {
          waitUntil: "domcontentloaded",
        });
        await page
          .getByLabel("Username", { exact: true })
          .waitFor({ state: "visible" });
        assert.equal(new URL(page.url()).origin, runtime.oauthIssuer);
        await signInIfPrompted(page, {
          username: runtime.adminUsername,
          password: runtime.adminPassword,
        });
        await waitForConsoleReady(page);
        assert.equal(new URL(page.url()).origin, runtime.publicOrigin);
        assert.deepEqual(errors, []);
      } finally {
        await context.close();
      }
    }, browserRuntimeOptions());
  },
);
