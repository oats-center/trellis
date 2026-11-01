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
  "a Console on its own origin signs in on the Trellis origin and returns to its own",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const context = await launchProfile(runtime);
        try {
          const page = await context.newPage();
          const errors: Error[] = [];
          page.on("pageerror", (error) => errors.push(error));
          await page.goto(`${runtime.consoleUrl}/console`, {
            waitUntil: "domcontentloaded",
          });
          await page
            .getByLabel("Username", { exact: true })
            .waitFor({ state: "visible" });
          assert.equal(new URL(page.url()).origin, runtime.trellisUrl);
          await signInIfPrompted(page, {
            username: runtime.adminUsername,
            password: runtime.adminPassword,
          });
          await waitForConsoleReady(page);
          assert.equal(new URL(page.url()).origin, runtime.consoleUrl);
          assert.deepEqual(errors, []);
        } finally {
          await context.close();
        }
      },
      browserRuntimeOptions({ ownConsoleOrigin: true }),
    );
  },
);
