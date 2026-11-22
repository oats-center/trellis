import assert from "node:assert/strict";
import { test } from "node:test";
import type { Locator, Page } from "playwright";
import { ulid } from "ulid";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { codecs } from "../packages/trellis/generated.ts";
import { API as authApi } from "../packages/trellis/internal_sdk/generated/apis/auth/mod.js";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  createOrdinaryUserWithPassword,
  launchProfile,
  startedServices,
  waitForConsoleReady,
} from "./browser_test_support.ts";
import { fixtureName, wireNatsGate } from "./console_test_support.ts";

// The Console checks its own authority again with this request each time the
// connection's authority changes or the connection comes back.
const USERS_RESOLVE = authApi.actions["rpc:Users.Resolve"].subject;
// Authority lasts a minute here instead of five, so the Console renews it on
// its own about every half minute.
const AUTHORITY_LIFETIME_SECONDS = 60;

type Draft = {
  /** The field the administrator typed into. */
  field: Locator;
  /** Puts this step's text in the field, as a person would type it. */
  enter: (step: string) => Promise<void>;
  /** What the field should still hold. */
  expected: (step: string) => string;
};

type OpenPage = {
  name: string;
  page: Page;
  wired: Awaited<ReturnType<typeof wireNatsGate>>;
  drafts: Draft[];
  /** Full document loads and URL changes since the drafts were typed. */
  navigations: string[];
  /** Resolves once whatever the page rereads after a check has settled. */
  settled: () => Promise<void>;
};

function typed(field: Locator, value: (step: string) => string): Draft {
  return {
    field,
    enter: (step) => field.fill(value(step)),
    expected: value,
  };
}

/** What `pages` lost of the drafts typed for `step`, one line per loss. */
async function lostDrafts(pages: OpenPage[], step: string): Promise<string[]> {
  const lost: string[] = [];
  for (const open of pages) {
    for (const draft of open.drafts) {
      // A short bound: the field is either still there with the text, or the
      // page lost it.
      const value = await draft.field
        .inputValue({ timeout: 2_000 })
        .catch(() => "(field gone)");
      if (value !== draft.expected(step))
        lost.push(
          `${open.name}: expected ${JSON.stringify(draft.expected(step))}, found ${JSON.stringify(value)}`,
        );
    }
    for (const navigation of open.navigations)
      lost.push(`${open.name}: navigated to ${navigation}`);
  }
  return lost;
}

/**
 * Holds the reply to each page's next authority check, runs `trigger`, and
 * checks every draft while the Console waits for that reply and again once it
 * has it.
 */
async function assertDraftsSurviveCheck(
  pages: OpenPage[],
  step: string,
  trigger: () => Promise<void>,
  timeoutMs = 10_000,
) {
  for (const open of pages)
    for (const draft of open.drafts) await draft.enter(step);
  const held = pages.map((open) =>
    open.wired.gate.armNextReply(USERS_RESOLVE, undefined, timeoutMs),
  );
  let during: string[];
  try {
    await trigger();
    await Promise.all(held);
    during = await lostDrafts(pages, step);
  } finally {
    for (const open of pages) open.wired.releaseToPage();
  }
  for (const open of pages) await open.settled();
  const after = await lostDrafts(pages, step);
  assert.deepEqual(
    { during, after },
    { during: [], after: [] },
    "drafts were lost while the Console rechecked its authority, or after",
  );
}

test("what an administrator types in the Console survives authority renewal, issuer rotation, a reconnect and live updates", async (t) => {
  const services = startedServices();
  try {
    await withTrellisRuntime(
      async (runtime) => {
        await runtime.ensureAdmin();
        const configured = await runtime.services.createInstance({
          name: "drafts",
          deployment: "drafts",
          contract: participants.ConfiguredService.participant,
        });
        const userId = await createOrdinaryUserWithPassword(runtime, {
          username: fixtureName("drafts-user"),
          password: "correct horse battery staple",
        });
        await runtime.callAdminRpc(
          "authRolesPut",
          authApi.actions["rpc:Roles.Put"].input.decode({
            expectedRevision: "0",
            title: `Draft readers ${ulid()}`,
            description: "",
            capabilityIds: ["trellis.auth.v1:users.read"],
            assignToNewUsers: false,
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );

        const context = await launchProfile(runtime);
        try {
          const pages: OpenPage[] = [];
          const open = async (
            name: string,
            path: string,
            drafts: (page: Page) => Promise<Draft[]>,
            settled: (page: Page) => Promise<void> = async () => {},
          ) => {
            const page = await context.newPage();
            const wired = await wireNatsGate(page);
            await page.goto(`${runtime.consoleUrl}/console${path}`, {
              waitUntil: "domcontentloaded",
            });
            await completeConsoleEntry(page, BROWSER_ADMIN);
            await waitForConsoleReady(page);
            const entry: OpenPage = {
              name,
              page,
              wired,
              drafts: await drafts(page),
              navigations: [],
              settled: async () => {
                await waitForConsoleReady(page);
                await settled(page);
              },
            };
            page.on("load", () => entry.navigations.push(`load ${page.url()}`));
            const url = page.url();
            page.on("framenavigated", (frame) => {
              if (frame === page.mainFrame() && frame.url() !== url)
                entry.navigations.push(frame.url());
            });
            pages.push(entry);
          };

          await open(
            "service settings",
            `/admin/services/${configured.deploymentId}#settings`,
            async (page) => {
              const settings = page.getByRole("region", { name: "Settings" });
              const endpoint = settings.getByLabel("Endpoint", { exact: true });
              await endpoint.waitFor();
              return [
                typed(endpoint, (step) => `https://${step}.example`),
                typed(
                  settings.getByLabel("Region", { exact: true }),
                  (step) => `region-${step}`,
                ),
              ];
            },
          );
          await open(
            "user",
            `/admin/users/edit?userId=${userId}`,
            async (page) => {
              const name = page.getByLabel("Display name", { exact: true });
              await name.waitFor();
              const role = page.getByLabel("Role to give", { exact: true });
              await role
                .locator("option")
                .nth(1)
                .waitFor({ state: "attached" });
              const roleId = await role
                .locator("option")
                .nth(1)
                .getAttribute("value");
              assert.ok(roleId);
              return [
                typed(name, (step) => `Draft ${step}`),
                {
                  field: role,
                  enter: async () => {
                    await role.selectOption(roleId);
                  },
                  expected: () => roleId,
                },
              ];
            },
            // The access panel rereads in place after each check.
            async (page) => {
              await page
                .getByRole("button", { name: "Refresh", exact: true })
                .and(page.locator(":enabled"))
                .waitFor();
            },
          );
          await open("new role", "/admin/roles/new", async (page) => {
            const title = page.getByLabel("Title", { exact: true });
            await title.waitFor();
            return [
              typed(title, (step) => `Role ${step}`),
              typed(
                page.getByLabel("Description", { exact: true }),
                (step) => `For ${step}`,
              ),
            ];
          });
          await open("new service", "/admin/services/new", async (page) => {
            const name = page.getByLabel("Name", { exact: true });
            await name.waitFor();
            return [typed(name, (step) => `service-${step}`)];
          });
          await open("events", "/admin/events?focus=all", async (page) => {
            const search = page.getByPlaceholder("Search events");
            await search.waitFor();
            return [typed(search, (step) => `search ${step}`)];
          });

          await t.test("the Console renewing its authority on its own", () =>
            assertDraftsSurviveCheck(
              pages,
              "renewal",
              async () => {},
              (AUTHORITY_LIFETIME_SECONDS + 15) * 1_000,
            ),
          );

          await t.test("an issuer rotation", () =>
            assertDraftsSurviveCheck(pages, "rotation", async () => {
              const metadata = await pages[0].page.evaluate(
                async (url) => (await fetch(url)).json(),
                `${runtime.trellisUrl}/.well-known/oauth-authorization-server`,
              );
              const signer = metadata.trellis.issuers.find(
                (key: { state: string }) => key.state === "active",
              ).keyId;
              await runtime.callAdminRpc("authIssuersRotate", {
                expectedCurrentKeyId: signer,
                idempotencyKey: codecs.ulid.decode(ulid()),
              });
            }),
          );

          await t.test("the connection dropping and coming back", () =>
            assertDraftsSurviveCheck(pages, "reconnect", async () => {
              await Promise.all(
                pages.map((open) => open.wired.dropConnection()),
              );
            }),
          );

          await t.test("live updates arriving", async () => {
            for (const open of pages)
              for (const draft of open.drafts) await draft.enter("live");
            // The service connects and waits for its settings; the service
            // page rereads its instances and shows that.
            services.add(
              TrellisService.connect({
                trellisUrl: runtime.trellisUrl,
                participant: participants.ConfiguredService.participant,
                seed: configured.seed,
                signal: services.signal,
              }).orThrow(),
            );
            await pages[0].page
              .getByText("Waiting for settings", { exact: true })
              .first()
              .waitFor({ timeout: 30_000 });
            // A service publishes events, which the Events page shows live.
            const name = fixtureName("drafts-provider");
            const key = await runtime.registerService({
              name,
              contract: participants.Provider.participant,
              deployment: name,
              acknowledgeSharedApis: true,
            });
            const provider = await TrellisService.connect({
              trellisUrl: runtime.trellisUrl,
              participant: participants.Provider.participant,
              name,
              seed: key.seed,
            }).orThrow();
            services.add(Promise.resolve(provider));
            const events = pages[4].page.locator(
              `main tbody tr[data-publisher="${key.deploymentId}"]`,
            );
            let publishing = true;
            const publisher = (async () => {
              for (let index = 0; publishing; index++) {
                await provider
                  .publishChanged({ value: `drafts-${index}` })
                  .orThrow();
                await new Promise((resolve) => setTimeout(resolve, 100));
              }
            })();
            try {
              await events.first().waitFor({ timeout: 30_000 });
            } finally {
              publishing = false;
              await publisher;
            }
            for (const open of pages) await open.settled();
            assert.deepEqual(
              await lostDrafts(pages, "live"),
              [],
              "drafts were lost while live updates arrived",
            );
          });
        } finally {
          await context.close();
        }
      },
      browserRuntimeOptions({
        authorization: { contextLifetimeSeconds: AUTHORITY_LIFETIME_SECONDS },
      }),
    );
  } finally {
    await services.stop();
  }
});
