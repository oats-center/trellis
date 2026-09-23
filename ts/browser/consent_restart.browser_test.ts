import { assert, assertEquals } from "@std/assert";

import type { TrellisTestRuntime } from "@oatscenter/trellis-test";

import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
  waitForConsoleConnected,
} from "./browser_test_support.ts";

const CONSOLE_PARTICIPANT = "trellis.console";

async function consoleBinding(runtime: TrellisTestRuntime, userId: string) {
  const response = await runtime.callAdminRpc("authGrantsList", {
    ownerId: userId,
    ownerKind: "user",
    page: { limit: 100 },
    participantId: CONSOLE_PARTICIPANT,
  });
  const binding = response.items.find(
    (item) => item.participantId === CONSOLE_PARTICIPANT,
  );
  assert(binding, "the ordinary user has a console binding");
  return binding;
}

Deno.test("ordinary user consent survives a control-plane restart and narrows under an override", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const user = {
      username: "restart-user",
      password: "restart-user-password",
    };
    const userId = await createOrdinaryUserWithPassword(runtime, user);

    const context = await launchProfile(runtime);
    try {
      const page = await context.newPage();
      await openConsole(page, runtime, user);
      await waitForConsoleConnected(page);

      const consented = await consoleBinding(runtime, userId);
      assert(
        consented.approval.approvedCapabilities.length > 0,
        "the ordinary consent approved a capability",
      );

      await runtime.restartControlPlane();
      await page.reload({ waitUntil: "domcontentloaded" });
      await waitForConsoleConnected(page);

      const survived = await consoleBinding(runtime, userId);
      assertEquals(survived.state, "active");
      assertEquals(
        survived.approval.approvedCapabilities,
        consented.approval.approvedCapabilities,
      );
      assertEquals(survived.grants.permissions, consented.grants.permissions);

      await runtime.ensurePortalConsentPolicy(CONSOLE_PARTICIPANT, []);
      const narrowed = await runtime.waitFor(async () => {
        const binding = await consoleBinding(runtime, userId);
        return binding.approval.approvedCapabilities.length <
            consented.approval.approvedCapabilities.length
          ? binding
          : null;
      }, { timeoutMs: 30_000 });
      assert(
        narrowed.approval.approvedCapabilities.length <
          consented.approval.approvedCapabilities.length,
        "the empty override reduces the approved scope",
      );
      assertEquals(narrowed.state, "active");
    } finally {
      await context.close();
    }
  }, browserRuntimeOptions());
});
