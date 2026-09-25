// Plaintext-HTTP browser acceptance: one ordinary insecure browser context uses
// the built-in console over the server-advertised ws:// runtime. No client-side
// insecure mode, no secure-origin browser flag, and no page-internal backend
// inspection are involved.

import { assertEquals } from "@std/assert";
import { ulid } from "ulid";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";
import {
  assertNoBrowserErrors,
  captureBrowserErrors,
  waitForConsoleShell,
} from "./console_test_support.ts";

/**
 * Returns the first non-loopback IPv4 address of the live test host.
 *
 * The document must be served from this host so Chromium classifies the page as
 * an ordinary insecure context while the request still reaches the local test
 * runtime.
 */
function nonLoopbackHost(): string {
  for (const iface of Deno.networkInterfaces()) {
    if (
      iface.family === "IPv4" &&
      !iface.address.startsWith("127.") &&
      !iface.address.startsWith("169.254.")
    ) {
      return iface.address;
    }
  }
  throw new Error(
    "an insecure-context browser test needs a non-loopback IPv4 address",
  );
}

Deno.test("a plaintext HTTP console signs in and reads a real RPC result", async () => {
  const browserHost = nonLoopbackHost();
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const username = `ct-http-${ulid().toLowerCase().slice(-10)}`;
    const created = await runtime.callAdminRpc("authUsersCreate", {
      email: null,
      idempotencyKey: ulid(),
      image: null,
      name: "Plaintext HTTP User",
      username,
    });

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      const errors = captureBrowserErrors(page);

      await page.goto(`${runtime.publicOrigin}/console`, {
        waitUntil: "domcontentloaded",
      });
      // Precondition, not implementation detail: the page is an ordinary
      // insecure context, so the browser provides no secure-context crypto.
      assertEquals(
        await page.evaluate(() => globalThis.isSecureContext),
        false,
      );

      await completeConsoleEntry(page, {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      });
      await waitForConsoleShell(page);

      // A real generated RPC read: the value seeded over the admin transport
      // must come back through the browser's own ws:// runtime connection.
      await page.goto(
        `${runtime.publicOrigin}/console/admin/users/edit?userId=${created.user.userId}`,
        { waitUntil: "domcontentloaded" },
      );
      await waitForConsoleShell(page);
      await page.getByText("Plaintext HTTP User").first().waitFor({
        state: "visible",
        timeout: 30_000,
      });
      assertNoBrowserErrors(errors);
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions({ browserHost }));
});
