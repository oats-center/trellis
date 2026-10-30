import { test } from "node:test";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

test("ordinary OAuth consent and account access survive a control-plane restart", async () => {
  await withTrellisRuntime(async (runtime) => {
    const user = {
      username: "restart-user",
      password: "restart-user-password",
    };
    await createOrdinaryUserWithPassword(runtime, user);
    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, user);
      await page.goto(`${runtime.consoleUrl}/console/profile`);
      await page
        .getByRole("heading", { name: user.username, exact: true })
        .waitFor()
        .catch(async (cause: unknown) => {
          throw new Error(
            `${page.url()}: ${await page.locator("body").innerText()}`,
            { cause },
          );
        });

      await runtime.restart();
      await page.reload({ waitUntil: "domcontentloaded" });
      await page
        .getByRole("heading", { name: user.username, exact: true })
        .waitFor();
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
