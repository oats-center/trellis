import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, readdir, writeFile } from "node:fs/promises";
import { hostname } from "node:os";
import { basename, join } from "node:path";
import { createInterface } from "node:readline";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import { promisify } from "node:util";
import { ulid } from "ulid";
import { chromium, type Page } from "playwright";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import { codecs } from "@oatscenter/trellis/generated";
import {
  discoverNativeOAuth,
  NativeOAuthClient,
} from "../packages/trellis/native.ts";
import { apis, apps } from "../packages/trellis-testkit/trellis/index.js";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  createOrdinaryUserWithPassword,
  launchProfile,
  loginCliAsAdministrator,
} from "./browser_test_support.ts";
import { startDex, type TestDex } from "./dex_support.ts";

/** Starts `trellis login` in its own configuration and reads its challenge. */
async function startCliLogin(runtime: TrellisTestRuntime) {
  const binary = process.env.TRELLIS_TEST_CLI_BIN;
  assert.ok(binary, "supply a producer-built CLI");
  const directory = join(runtime.workdir, `cli-${ulid()}`);
  await mkdir(directory);
  const env = { ...process.env, XDG_CONFIG_HOME: directory };
  const login = spawn(
    binary,
    [
      "--format",
      "json",
      "login",
      runtime.trellisUrl,
      "--ca-certificate",
      runtime.caCertificatePath,
    ],
    { env, stdio: ["ignore", "pipe", "pipe"] },
  );
  const exited = once(login, "exit");
  const progress = createInterface({ input: login.stderr });
  let challenge: { loginUrl: string; userCode: string } | undefined;
  for await (const line of progress) {
    if (line.startsWith("{")) {
      challenge = JSON.parse(line);
      break;
    }
  }
  assert.ok(challenge);
  const finished = async () => {
    try {
      return (await exited)[0] as number | null;
    } finally {
      progress.close();
    }
  };
  return {
    ...challenge,
    /** The principal the CLI's stored login authenticates as. */
    async principal(): Promise<string> {
      assert.equal(await finished(), 0);
      const { stdout } = await promisify(execFile)(
        binary,
        ["--format", "json", "whoami"],
        { env, timeout: 15_000 },
      );
      return JSON.parse(stdout).principal.principalId;
    },
    /** The CLI's exit code once it gives up or finishes. */
    exitCode: finished,
    stop: () => login.kill("SIGTERM"),
  };
}

/** Types `code` on the code step and continues. */
async function typeCode(page: Page, code: string): Promise<void> {
  await page.getByRole("heading", { name: "Connect a device" }).waitFor();
  await page.getByLabel("Code", { exact: true }).fill(code);
  await page.getByRole("button", { name: "Continue", exact: true }).click();
}

/** Checks the device details the confirmation step shows for the CLI. */
async function checkDetails(page: Page, userCode: string): Promise<void> {
  await page.getByRole("heading", { name: "Is this your device?" }).waitFor();
  const details = page.locator(".device-details");
  const detail = (term: string) =>
    details.locator(`dt:text-is("${term}") + dd`).innerText();
  assert.equal(await detail("App"), "Trellis CLI");
  assert.match(await detail("Kind"), /^Installed app/);
  assert.equal(await detail("Code"), userCode);
  assert.match(await detail("Requested"), /^(Just now|1 minute ago)$/);
  // The CLI asked from this machine, and names it by its hostname.
  assert.match(await detail("From"), /^(127\.0\.0\.1|::1)$/);
  assert.equal(await detail("Device says it is"), hostname());
}

/** Allows what the consent page offers and waits for the done page. */
async function allow(page: Page): Promise<void> {
  await page
    .getByRole("heading", { name: /wants to use your account/ })
    .waitFor();
  await page.getByRole("button", { name: "Allow", exact: true }).click();
  await page
    .getByRole("heading", { name: "Your device is connected" })
    .waitFor();
  await page.getByText("You can close this window.", { exact: true }).waitFor();
}

test("a Device Code sign-in takes the account, then the code, then the device's details, then the consent", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const avery = { username: "avery", password: "avery-portal-password" };
    const averyId = await createOrdinaryUserWithPassword(runtime, {
      ...avery,
      name: "Avery Able",
    });
    const context = await launchProfile(runtime);
    const logins: { stop(): void }[] = [];
    try {
      const page = await context.newPage();
      page.setDefaultTimeout(30_000);

      // Not signed in: the device's link signs in first, then lands straight
      // on the code step.
      const first = await startCliLogin(runtime);
      logins.push(first);
      await page.goto(first.loginUrl);
      await page.getByLabel("Username", { exact: true }).fill(avery.username);
      await page.getByLabel("Password", { exact: true }).fill(avery.password);
      await page.getByRole("button", { name: "Sign in", exact: true }).click();
      await typeCode(page, first.userCode);
      await checkDetails(page, first.userCode);
      await page
        .getByRole("button", { name: "Yes, this is my device", exact: true })
        .click();
      await allow(page);
      assert.equal(await first.principal(), averyId);

      // Signed in: the link shows who, and continues as them on request.
      const second = await startCliLogin(runtime);
      logins.push(second);
      await page.goto(second.loginUrl);
      await page.getByRole("heading", { name: "Device activation" }).waitFor();
      const account = page.locator(".account");
      await account.getByText("Avery Able", { exact: true }).waitFor();
      await account.getByText("avery", { exact: true }).waitFor();
      // Initials stand in for a profile picture the account doesn't have.
      assert.equal(await account.locator(".avatar").innerText(), "AA");
      await page.getByRole("link", { name: "Continue", exact: true }).click();
      const codeStep = new URL(page.url());
      // The consent can't be reached before the code: it leads back here.
      await page.goto(
        `${runtime.trellisUrl}/consent?${codeStep.searchParams.toString()}`,
      );
      await page.getByRole("heading", { name: "Connect a device" }).waitFor();
      await typeCode(page, second.userCode);
      await checkDetails(page, second.userCode);
      // Nor before the device is confirmed: the consent page leads back to
      // the details, and the Portal refuses an approval sent anyway.
      await page.goto(
        `${runtime.trellisUrl}/consent?${codeStep.searchParams.toString()}`,
      );
      await page
        .getByRole("heading", { name: "Is this your device?" })
        .waitFor();
      const skipped = await page.evaluate(
        async (transaction) => {
          const csrf =
            document.cookie
              .split("; ")
              .find((cookie) =>
                cookie.startsWith("__Host-trellis-portal-csrf="),
              )
              ?.split("=")[1] ?? "";
          const approve = await fetch("/portal/approve", {
            method: "POST",
            body: new URLSearchParams({
              csrf,
              transaction,
              decision: "approve",
            }),
          });
          const consent = await fetch(`/portal/authorization/${transaction}`);
          return [approve.status, consent.status];
        },
        codeStep.searchParams.get("transaction") ?? "",
      );
      assert.deepEqual(skipped, [400, 400]);
      await page
        .getByRole("button", { name: "Yes, this is my device", exact: true })
        .click();
      await allow(page);
      assert.equal(await second.principal(), averyId);

      // Unknown and used codes get one identical page.
      const invalid = async (code: string) => {
        await page.goto(`${runtime.trellisUrl}/device`);
        await page.getByRole("link", { name: "Continue", exact: true }).click();
        await typeCode(page, code);
        await page.getByRole("heading", { name: "Check the code" }).waitFor();
        return await page.locator("main").innerHTML();
      };
      const used = await invalid(second.userCode);
      assert.match(used, /This code isn't valid\./);
      assert.equal(await invalid("ZZZZ-ZZZZ"), used);

      // Cancelling at the device's details gives the CLI nothing.
      const cancelled = await startCliLogin(runtime);
      logins.push(cancelled);
      await page.goto(cancelled.loginUrl);
      await page.getByRole("link", { name: "Continue", exact: true }).click();
      await typeCode(page, cancelled.userCode);
      await checkDetails(page, cancelled.userCode);
      await page.getByRole("button", { name: "Cancel", exact: true }).click();
      await page
        .getByRole("heading", { name: "Device not connected" })
        .waitFor();
      assert.notEqual(await cancelled.exitCode(), 0);
    } finally {
      for (const login of logins) login.stop();
      await context.close();
    }
  }, browserRuntimeOptions());
});

/** How many OAuth grants Auth holds for `principalId`. */
async function grantsFor(
  runtime: TrellisTestRuntime,
  principalId: string,
): Promise<number> {
  const files = await readdir(runtime.workdir, { recursive: true });
  const path = files.find((file) => basename(file) === "platform.sqlite");
  assert(path, files.join("\n"));
  const database = new DatabaseSync(join(runtime.workdir, path), {
    timeout: 10_000,
  });
  try {
    const row = database
      .prepare(
        "SELECT count(*) AS grants FROM auth_oauth_grants WHERE principal_id=?",
      )
      .get(principalId) as { grants: number };
    return row.grants;
  } finally {
    database.close();
  }
}

/** Creates an account from the Portal's sign-in page, with a fresh username. */
async function createAccount(page: Page, name: string): Promise<string> {
  const username = `${name.toLowerCase()}-${ulid().toLowerCase()}`;
  await page
    .getByRole("button", { name: "Create an account", exact: true })
    .click();
  await page.getByLabel("Name", { exact: true }).fill(name);
  await page.getByLabel("Username", { exact: true }).fill(username);
  await page
    .getByLabel("Password", { exact: true })
    .fill("a newcomer's portal password");
  await page
    .getByRole("button", { name: "Create account", exact: true })
    .click();
  return username;
}

// The account step comes before the code, so it can't know the device's app.
// It offers sign-up when the installation allows it and some Device Code app
// does; the code then names the app, and an app that doesn't allow sign-up is
// refused for the account just created, which gets nothing for that app. This
// catches sign-up that is never offered to a device, and an account created
// for one app that slips through to another that doesn't allow sign-up.
test("a Device Code sign-in offers sign-up when a Device Code app allows it, and the code step refuses an app that doesn't", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const browser = await chromium.launch({
      headless: true,
      env: browserTrustEnvironment(),
    });
    const logins: { stop(): void }[] = [];
    try {
      const fresh = async () => {
        const page = await (await browser.newContext()).newPage();
        page.setDefaultTimeout(30_000);
        return page;
      };
      const { run } = await loginCliAsAdministrator(runtime, await fresh());
      await run("sign-in", "set", "--local-sign-up");
      // A Device Code app that never lets people create an account.
      await runtime.callAdminRpc("authOAuthClientsPut", {
        clientId: "acme.closed-tool",
        kind: "native",
        title: "Closed Tool",
        description: "A tool for existing accounts only",
        redirectUris: [],
        development: false,
        impliedConsent: [],
        expectedRevision: 0n,
        idempotencyKey: codecs.ulid.decode(ulid()),
      });

      // No Device Code app allows sign-up yet, so none is offered.
      const closed = await startCliLogin(runtime);
      logins.push(closed);
      const nobody = await fresh();
      await nobody.goto(closed.loginUrl);
      await nobody.getByLabel("Username", { exact: true }).waitFor();
      assert.equal(
        await nobody.getByRole("button", { name: "Create an account" }).count(),
        0,
      );
      closed.stop();

      // The CLI allows it: a newcomer creates an account and connects it.
      await run("clients", "sign-up", "trellis.cli", "--password");
      const open = await startCliLogin(runtime);
      logins.push(open);
      const newcomer = await fresh();
      await newcomer.goto(open.loginUrl);
      const username = await createAccount(newcomer, "Newcomer");
      await typeCode(newcomer, open.userCode);
      await checkDetails(newcomer, open.userCode);
      await newcomer
        .getByRole("button", { name: "Yes, this is my device", exact: true })
        .click();
      await allow(newcomer);
      const created = (await run("users", "show", username)).principalId;
      assert.equal(await open.principal(), created);

      // The closed tool doesn't: the code step refuses the account created
      // for it, and the account stays with nothing granted for the tool.
      const server = (await discoverNativeOAuth(runtime.trellisUrl)).orThrow();
      const tool = (
        await NativeOAuthClient.open({
          scope: {
            issuer: server.issuer,
            instanceId: server.trust.instanceId,
            account: server.trust.account,
            clientId: "acme.closed-tool",
            credentialHandle: ulid(),
          },
          trust: server.trust,
        })
      ).orThrow();
      try {
        const device = (await tool.startDevice(apps.cli.request)).orThrow();
        const latecomer = await fresh();
        await latecomer.goto(device.verificationUri);
        const refusedName = await createAccount(latecomer, "Latecomer");
        await typeCode(latecomer, device.userCode);
        await latecomer
          .getByRole("heading", { name: "This app doesn't allow new accounts" })
          .waitFor();
        await latecomer
          .getByText(/^Closed Tool doesn't let people create an account/)
          .waitFor();
        const refused = (await run("users", "show", refusedName)).principalId;
        assert.equal(await grantsFor(runtime, refused), 0);
      } finally {
        await tool.close();
      }
    } finally {
      for (const login of logins) login.stop();
      await browser.close();
    }
  }, browserRuntimeOptions());
});

/** Signs in at Dex as `email`, from the Portal's sign-in page `page` shows. */
async function continueWithDex(
  page: Page,
  dex: TestDex,
  email: string,
): Promise<void> {
  await page
    .getByRole("link", { name: "Continue with Company", exact: true })
    .click();
  await page.waitForURL((url) => url.origin === new URL(dex.issuer).origin);
  await page.locator('input[name="login"]').fill(email);
  await page.locator('input[name="password"]').fill("password");
  await page.locator('button[type="submit"]').click();
}

// The same holds for an account created by signing in through an OIDC
// provider: the account step offers it when some Device Code app allows
// provider sign-up, and the code step refuses the account for an app that
// doesn't. This catches provider sign-up that a device never gets, sign-up
// offered when no Device Code app allows it, and an account created through
// the provider for one app that slips through to another.
test(
  "a Device Code sign-in creates an account through an OIDC provider when a Device Code app allows it, and the code step refuses an app that doesn't",
  { timeout: 300_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      await runtime.ensureAdmin();
      const emails = {
        early: "early@example.com",
        newcomer: "newcomer@example.com",
        latecomer: "latecomer@example.com",
      };
      const dex = await startDex({
        directory: runtime.workdir,
        redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
        emails: Object.values(emails),
      });
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const logins: { stop(): void }[] = [];
      try {
        const fresh = async () => {
          const page = await (await browser.newContext()).newPage();
          page.setDefaultTimeout(30_000);
          return page;
        };
        const { run } = await loginCliAsAdministrator(runtime, await fresh());
        const secretFile = join(runtime.workdir, `dex-secret-${ulid()}`);
        await writeFile(secretFile, dex.clientSecret);
        const { provider } = await run(
          "sign-in",
          "providers",
          "add",
          "--name",
          "Company",
          "--issuer",
          dex.issuer,
          "--client-id",
          dex.clientId,
          "--client-secret-file",
          secretFile,
          "--certificate-fingerprint",
          dex.fingerprint,
        );
        /** The account linked to `email`'s Dex sign-in, if there is one. */
        const owner = async (email: string) =>
          (
            await runtime.callAdminRpc(
              "authUsersFind",
              apis.auth.API.actions["rpc:Users.Find"].input.decode({
                identity: {
                  providerId: provider.providerId,
                  subject: dex.subjects.get(email),
                },
              }),
            )
          ).found?.user.principalId;
        // A Device Code app that never lets people create an account.
        await runtime.callAdminRpc("authOAuthClientsPut", {
          clientId: "acme.closed-tool",
          kind: "native",
          title: "Closed Tool",
          description: "A tool for existing accounts only",
          redirectUris: [],
          development: false,
          impliedConsent: [],
          expectedRevision: 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });

        // No Device Code app allows provider sign-up yet, so signing in
        // through the provider creates nothing.
        const closed = await startCliLogin(runtime);
        logins.push(closed);
        const early = await fresh();
        await early.goto(closed.loginUrl);
        await continueWithDex(early, dex, emails.early);
        await early
          .getByText(/No Trellis account is linked to this provider account/)
          .waitFor();
        assert.equal(await owner(emails.early), undefined);
        closed.stop();

        // The CLI allows it: a newcomer creates an account through the
        // provider and connects the CLI as it.
        await run("clients", "sign-up", "trellis.cli", "--providers");
        const open = await startCliLogin(runtime);
        logins.push(open);
        const newcomer = await fresh();
        await newcomer.goto(open.loginUrl);
        await continueWithDex(newcomer, dex, emails.newcomer);
        await typeCode(newcomer, open.userCode);
        await checkDetails(newcomer, open.userCode);
        await newcomer
          .getByRole("button", { name: "Yes, this is my device", exact: true })
          .click();
        await allow(newcomer);
        const created = await owner(emails.newcomer);
        assert.ok(created, "the provider sign-in created an account");
        assert.equal(await open.principal(), created);

        // The closed tool doesn't: the code step refuses the account created
        // through the provider for it, which gets nothing for the tool.
        const server = (
          await discoverNativeOAuth(runtime.trellisUrl)
        ).orThrow();
        const tool = (
          await NativeOAuthClient.open({
            scope: {
              issuer: server.issuer,
              instanceId: server.trust.instanceId,
              account: server.trust.account,
              clientId: "acme.closed-tool",
              credentialHandle: ulid(),
            },
            trust: server.trust,
          })
        ).orThrow();
        try {
          const device = (await tool.startDevice(apps.cli.request)).orThrow();
          const latecomer = await fresh();
          await latecomer.goto(device.verificationUri);
          await continueWithDex(latecomer, dex, emails.latecomer);
          await typeCode(latecomer, device.userCode);
          await latecomer
            .getByRole("heading", {
              name: "This app doesn't allow new accounts",
            })
            .waitFor();
          const refused = await owner(emails.latecomer);
          assert.ok(refused, "the account stays, as an ordinary account");
          assert.equal(await grantsFor(runtime, refused), 0);
        } finally {
          await tool.close();
        }
      } finally {
        for (const login of logins) login.stop();
        await browser.close();
        await dex.stop();
      }
    }, browserRuntimeOptions());
  },
);
