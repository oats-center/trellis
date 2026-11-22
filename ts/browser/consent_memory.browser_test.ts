// What a person answers on the consent page is kept, and access an
// administrator gives later reaches the Console at its next renewal.
// Real runtime, real Portal, real Console, real browser app.

import assert from "node:assert/strict";
import { once } from "node:events";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { type Page } from "playwright";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { build } from "vite";

import {
  apps,
  participants,
} from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { Result } from "../packages/result/mod.ts";
import { apis } from "../packages/trellis-testkit/trellis/index.js";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  approveConsentIfRequired,
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  openConsole,
} from "./browser_test_support.ts";

type Module = {
  TrellisClient: typeof import("../packages/trellis/client_connect.ts").TrellisClient;
  base: typeof import("../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js").apps;
};
type Connected =
  import("../packages/trellis/client_connect.ts").ConnectedTrellisClient<
    typeof apps.OptionalCaller.request
  >;

const request = apps.OptionalCaller.request;
const [extra] = request.optionalCapabilities;

// The app: a module with the SDK and the fixture's request, a blank page for
// each callback, and `/use`, which offers a button that calls Extra and asks
// for it in a popup when it isn't approved.
let directory = "";
let server: Server;
let origin = "";
const issued = { issuer: "", clientId: "" };

before(async () => {
  directory = await mkdtemp(join(tmpdir(), "trellis-consent-memory-"));
  const root = fileURLToPath(new URL("../../", import.meta.url));
  const entry = join(directory, "entry.ts");
  await writeFile(
    entry,
    `
    export { TrellisClient } from ${JSON.stringify(`${root}/ts/packages/trellis/client_connect.ts`)};
    export { apps as base } from ${JSON.stringify(`${root}/integration/fixtures/client-upgrade/packages/client-upgrade/index.js`)};
  `,
  );
  await build({
    configFile: false,
    logLevel: "error",
    build: {
      outDir: directory,
      emptyOutDir: false,
      lib: { entry, formats: ["es"], fileName: () => "app.js" },
      rollupOptions: { output: { inlineDynamicImports: true } },
    },
  });
  const source = await readFile(join(directory, "app.js"));
  const use = () => `
    const module = await import("/app.js");
    const client = (
      await module.TrellisClient.connect({
        trellisUrl: ${JSON.stringify(issued.issuer)},
        request: module.base.OptionalCaller.request,
        auth: {
          clientId: ${JSON.stringify(issued.clientId)},
          redirectTo: location.origin + "/approved",
          approval: "popup",
        },
      })
    ).unwrapOrElse((error) => { throw error; });
    const button = document.createElement("button");
    button.id = "extra";
    button.textContent = "Extra";
    button.onclick = () => {
      window.clicked = client.extra({ value: "used" }).then(
        (result) => result.isOk() ? "approved" : "failed",
      );
    };
    document.body.replaceChildren(button);
    window.closeClient = () => client.connection.close();
    console.log("use ready");
  `;
  // Where the approval popup lands: it connects the way any page of the app
  // does, which hands the outcome to the window that opened it.
  const approved = () => `
    const module = await import("/app.js");
    await module.TrellisClient.connect({
      trellisUrl: ${JSON.stringify(issued.issuer)},
      request: module.base.OptionalCaller.request,
      auth: {
        clientId: ${JSON.stringify(issued.clientId)},
        redirectTo: location.origin + "/approved",
      },
    });
  `;
  server = createServer((incoming, response) => {
    const path = new URL(incoming.url ?? "/", "http://localhost").pathname;
    const script = path === "/app.js";
    response.writeHead(200, {
      "Content-Type": script ? "text/javascript" : "text/html",
    });
    const page =
      path === "/use" ? use() : path === "/approved" ? approved() : "";
    response.end(
      script
        ? source
        : `<!doctype html><title>Consent memory</title><script type="module">${page}</script>`,
    );
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  assert(address && typeof address !== "string");
  origin = `http://127.0.0.1:${address.port}`;
});

after(async () => {
  await new Promise<void>((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
  await rm(directory, { recursive: true, force: true });
});

/** Registers the app as an ordinary one, allowed nothing without asking. */
async function registerApp(runtime: TrellisTestRuntime): Promise<void> {
  const clientId = ulid();
  Object.assign(issued, { issuer: runtime.trellisUrl, clientId });
  await runtime.callAdminRpc("authOAuthClientsPut", {
    clientId,
    kind: "browser",
    title: "Consent memory",
    description: "Real browser consent memory acceptance",
    redirectUris: [`${origin}/callback`, `${origin}/approved`],
    development: true,
    impliedConsent: [],
    expectedRevision: 0n,
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
}

/** Gives `principalId` `capabilityId` directly. */
async function grant(
  runtime: TrellisTestRuntime,
  principalId: Awaited<ReturnType<typeof createOrdinaryUserWithPassword>>,
  capabilityId: string,
): Promise<void> {
  await runtime.callAdminRpc("authCapabilityGrantsGrant", {
    principalId,
    capabilityId,
    expectedRevision: 0n,
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
}

/** Starts the provider the app calls. */
async function startProvider(runtime: TrellisTestRuntime) {
  const identity = await runtime.registerService({
    name: `consent-memory-${ulid()}`.toLowerCase(),
    contract: participants.Provider.participant,
  });
  const service = await TrellisService.connect({
    trellisUrl: runtime.trellisUrl,
    participant: participants.Provider.participant,
    seed: identity.seed,
  }).orThrow();
  const exited = service.wait().catch(() => undefined);
  await service.handleEcho(({ input }) => Result.ok(input));
  await service.handleExtra(({ input }) => Result.ok(input));
  return {
    async stop() {
      await service.stop();
      await exited;
    },
  };
}

/**
 * Connects the app on `page` at its callback, keeping the client on
 * `window.client`. Returns the sign-in URL instead when it has no grant.
 */
async function connectApp(
  page: Page,
): Promise<{ loginUrl: string } | { declined: boolean }> {
  return await page.evaluate(
    async ({ issuer, origin, clientId, extra }) => {
      const module: Module = await import(`${origin}/app.js`);
      let loginUrl = "";
      const connected = await module.TrellisClient.connect({
        trellisUrl: issuer,
        request: module.base.OptionalCaller.request,
        auth: { clientId, redirectTo: `${origin}/callback` },
        onAuthRequired: (required) => {
          loginUrl = required.loginUrl;
        },
      });
      if (connected.isErr()) {
        if (loginUrl) return { loginUrl };
        throw connected.error;
      }
      const client: Connected = connected.unwrapOrElse((error) => {
        throw error;
      });
      Reflect.set(window, "client", client);
      // A later connect on this page is not this callback again.
      history.replaceState(null, "", location.pathname);
      return {
        declined: client.availability().declined[extra] === true,
      };
    },
    { ...issued, origin, extra },
  );
}

/** Ends the app's grant on `page`, as signing out of just the app does. */
async function signOutOfApp(page: Page): Promise<void> {
  await page.evaluate(async () => {
    const client: Connected = Reflect.get(window, "client");
    await client.logout();
  });
}

/**
 * Follows `loginUrl` back to the app's callback, completing sign-in with
 * `credentials` when asked, and reports whether the consent page was shown.
 * With `answer`, the page must be shown, and `answer` fills it in.
 */
async function signIn(
  page: Page,
  loginUrl: string,
  options: {
    credentials?: { username: string; password: string };
    answer?: (page: Page) => Promise<void>;
  } = {},
): Promise<{ askedForConsent: boolean }> {
  let askedForConsent = false;
  // The page that only asks a signed-in person whether to continue as
  // themselves asks nothing about access.
  const watch = (frame: { url(): string }) => {
    const shown = new URL(frame.url());
    if (
      shown.pathname === "/consent" &&
      shown.searchParams.get("account") !== "confirm"
    )
      askedForConsent = true;
  };
  page.on("framenavigated", watch);
  try {
    await page.goto(loginUrl);
    if (options.credentials) {
      await page
        .getByLabel("Username", { exact: true })
        .fill(options.credentials.username);
      await page
        .getByLabel("Password", { exact: true })
        .fill(options.credentials.password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
    }
    if (options.answer) await options.answer(page);
    const returned = page
      .waitForURL(
        (url) => url.origin === origin && url.pathname === "/callback",
      )
      .then(() => "returned" as const);
    const continueAs = page.getByRole("link", { name: /^Continue as / });
    if (
      (await Promise.race([
        returned,
        continueAs.waitFor().then(() => "account" as const),
      ])) === "account"
    ) {
      await continueAs.click();
      await returned;
    }
  } finally {
    page.off("framenavigated", watch);
  }
  return { askedForConsent };
}

// A decline left on the consent page is kept without a "remember" switch, so
// the next sign-in doesn't ask again; the app asking at the moment the person
// uses it shows the page; and the approval given there is kept too. This
// catches a decline that brings the page back at every sign-in, an approval
// that is forgotten, and an app that can never ask again once declined.
test("consent answers are kept: a decline skips the page at sign-in until the app asks at use, and the approval is kept", async () => {
  await withTrellisRuntime(async (runtime) => {
    const provider = await startProvider(runtime);
    const profile = await launchProfile(runtime);
    try {
      await registerApp(runtime);
      const {
        user: { principalId },
      } = await runtime.callAdminRpc("authUsersResolve", {});
      for (const capabilityId of new Set([
        ...request.requiredCapabilities,
        ...request.optionalCapabilities,
      ]))
        await grant(runtime, principalId, capabilityId);
      const credentials = {
        username: runtime.adminUsername,
        password: runtime.adminPassword,
      };
      const page = await profile.newPage();
      await page.goto(`${origin}/callback`);

      // First sign-in: what the app requires is listed without a choice, and
      // the optional capability starts selected. The person clears it.
      const first = await connectApp(page);
      assert("loginUrl" in first);
      const declining = await signIn(page, first.loginUrl, {
        credentials,
        answer: async (page) => {
          const allow = page.getByRole("button", {
            name: "Allow",
            exact: true,
          });
          await allow.waitFor();
          const documents = page.getByRole("checkbox", { name: /Documents/i });
          assert.equal(await documents.first().isChecked(), true);
          assert.deepEqual(
            await page
              .getByRole("checkbox")
              .evaluateAll((boxes) =>
                boxes.map((box) => (box as HTMLInputElement).checked),
              ),
            [true, true],
            "the optional capability's checkboxes start checked",
          );
          for (const name of [/Echo/, /remember/i])
            assert.equal(
              await page.getByRole("checkbox", { name }).count(),
              0,
              `no checkbox named ${name}`,
            );
          await page
            .getByText("Approving lets it", { exact: true })
            .locator("xpath=../following-sibling::ul[1]")
            .getByText("Echo", { exact: true })
            .waitFor();
          await page
            .getByRole("checkbox", { name: "Allow all: Documents" })
            .uncheck();
          await allow.click();
        },
      });
      assert.equal(declining.askedForConsent, true);
      assert.deepEqual(await connectApp(page), { declined: true });
      await signOutOfApp(page);

      // Signing in again doesn't ask: the decline is kept.
      const second = await connectApp(page);
      assert("loginUrl" in second);
      assert.deepEqual(await signIn(page, second.loginUrl), {
        askedForConsent: false,
      });
      assert.deepEqual(await connectApp(page), { declined: true });
      await page.evaluate(async () => {
        const client: Connected = Reflect.get(window, "client");
        await client.connection.close();
      });

      // Using what was declined asks, in a popup, and the approval there
      // lets the call through.
      const use = await profile.newPage();
      const ready = use.waitForEvent("console", {
        predicate: (message) => message.text() === "use ready",
      });
      await use.goto(`${origin}/use`);
      await ready;
      const opened = use.waitForEvent("popup");
      await use.click("#extra");
      const popup = await opened;
      await popup
        .getByRole("button", { name: "Allow", exact: true })
        .waitFor({ timeout: 15_000 });
      // It starts as the person left it: declined, so unchecked.
      assert.equal(
        await popup
          .getByRole("checkbox", { name: "Allow all: Documents" })
          .isChecked(),
        false,
      );
      await approveConsentIfRequired(popup, 15_000);
      assert.equal(
        await use.evaluate(async () => {
          const outcome: unknown = await Promise.race([
            Reflect.get(window, "clicked"),
            new Promise((resolve) =>
              setTimeout(() => resolve("still waiting"), 30_000),
            ),
          ]);
          await Reflect.get(window, "closeClient")();
          return outcome;
        }),
        "approved",
      );
      await use.close();

      // The approval is kept: a fresh sign-in doesn't ask and has it.
      await page.goto(`${origin}/callback`);
      assert.deepEqual(await connectApp(page), { declined: false });
      await signOutOfApp(page);
      const third = await connectApp(page);
      assert("loginUrl" in third);
      assert.deepEqual(await signIn(page, third.loginUrl), {
        askedForConsent: false,
      });
      assert.deepEqual(await connectApp(page), { declined: false });
    } finally {
      await profile.close();
      await provider.stop();
    }
  }, browserRuntimeOptions());
});

const AUTHORITY_LIFETIME_SECONDS = 20;

// An administrator gives a person a role while the Console is open. The
// Console allows the role's access without asking, so it reaches the open
// Console at its next renewal, administration included, with no new sign-in.
// An ordinary app the person signed in to before doesn't gain it: they were
// never asked. This catches renewals that only ever take access away.
test("a role assigned while the Console is open reaches it at the next renewal, but not an app that must ask", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const provider = await startProvider(runtime);
      const profile = await launchProfile(runtime);
      try {
        await registerApp(runtime);
        const credentials = {
          username: `role-arrival-${ulid()}`.toLowerCase(),
          password: `Role-arrival-${ulid()}`,
        };
        const principalId = await createOrdinaryUserWithPassword(
          runtime,
          credentials,
        );
        for (const capabilityId of request.requiredCapabilities)
          await grant(runtime, principalId, capabilityId);

        const consolePage = await profile.newPage();
        await openConsole(consolePage, runtime, credentials);
        const users = consolePage.getByRole("link", {
          name: "Users",
          exact: true,
        });
        assert.equal(await users.count(), 0);

        // The app asks for Extra, which the person can't have yet, so the
        // page offers only what the app requires.
        const app = await profile.newPage();
        await app.goto(`${origin}/callback`);
        const signing = await connectApp(app);
        assert("loginUrl" in signing);
        await signIn(app, signing.loginUrl, {
          answer: (page) => approveConsentIfRequired(page, 15_000),
        });
        assert.deepEqual(await connectApp(app), { declined: true });

        const role = (
          await runtime.callAdminRpc(
            "authRolesPut",
            apis.auth.API.actions["rpc:Roles.Put"].input.decode({
              expectedRevision: "0",
              title: `Account managers ${ulid()}`,
              description: "",
              capabilityIds: ["trellis.auth.v1:accounts.manage", extra],
              assignToNewUsers: false,
              idempotencyKey: codecs.ulid.decode(ulid()),
            }),
          )
        ).role;
        await runtime.callAdminRpc(
          "authRoleAssignmentsAssign",
          apis.auth.API.actions["rpc:RoleAssignments.Assign"].input.decode({
            principalId,
            roleId: role.roleId,
            expectedRevision: "0",
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );
        await app.evaluate(() => {
          const client: Connected = Reflect.get(window, "client");
          Reflect.set(window, "assigned", client.ownAuthority.current);
        });

        // The Console shows the page the role allows, without leaving it.
        const signedIn = consolePage.url();
        await users.waitFor({ timeout: 3 * AUTHORITY_LIFETIME_SECONDS * 1000 });
        assert.equal(consolePage.url(), signedIn);
        await users.click();
        await consolePage
          .locator("main")
          .getByText(credentials.username, { exact: true })
          .first()
          .waitFor();

        // The app renewed after the assignment, without Extra.
        await app.waitForFunction(
          () => {
            const client: Connected = Reflect.get(window, "client");
            return (
              client.ownAuthority.current !== undefined &&
              client.ownAuthority.current !== Reflect.get(window, "assigned")
            );
          },
          undefined,
          { timeout: 3 * AUTHORITY_LIFETIME_SECONDS * 1000 },
        );
        assert.deepEqual(
          await app.evaluate((extra) => {
            const client: Connected = Reflect.get(window, "client");
            return {
              held: (client.ownAuthority.current?.capabilities ?? []).some(
                (capability) => capability.capabilityId === extra,
              ),
              declined: client.availability().declined[extra] === true,
            };
          }, extra),
          { held: false, declined: true },
        );
      } finally {
        await profile.close();
        await provider.stop();
      }
    },
    browserRuntimeOptions({
      authorization: { contextLifetimeSeconds: AUTHORITY_LIFETIME_SECONDS },
    }),
  );
});
