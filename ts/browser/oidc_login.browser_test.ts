import { assert, assertEquals } from "@std/assert";
import { createServer } from "node:http";
import { generateKeyPairSync } from "node:crypto";
// @deno-types="npm:@types/oidc-provider@9.12.1"
import Provider from "npm:oidc-provider@9.12.2";
import { ulid } from "ulid";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
  waitForConsoleConnected,
} from "./browser_test_support.ts";

Deno.test("browser OIDC starts from an intent, verifies the real provider callback, and completes portal consent", async () => {
  let provider: Provider;
  const server = createServer((request, response) =>
    provider.callback()(request, response)
  );
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  assert(address && typeof address !== "string");
  const issuer = `http://127.0.0.1:${address.port}`;
  try {
    const { privateKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
    provider = new Provider(issuer, {
      jwks: {
        keys: [{
          ...privateKey.export({ format: "jwk" }),
          kid: "oidc-browser",
          use: "sig",
          alg: "RS256",
        }],
      },
      cookies: { keys: [crypto.randomUUID()] },
      features: {
        devInteractions: { enabled: false },
        registration: { enabled: true },
        registrationManagement: { enabled: true },
      },
      interactions: {
        url: (_context, interaction) => `/interaction/${interaction.uid}`,
      },
      findAccount: (_context, accountId) =>
        accountId === "oidc-user"
          ? {
            accountId,
            claims: () => ({
              sub: accountId,
              email: "oidc-user@example.com",
              email_verified: true,
            }),
          }
          : undefined,
    });
    // This is an ordinary provider integration: the library owns OAuth state,
    // PKCE, codes, signed tokens and nonce claims; the application owns its login UI.
    provider.use(async (context, next) => {
      if (!context.path.startsWith("/interaction/")) return await next();
      const details = await provider.interactionDetails(
        context.req,
        context.res,
      );
      if (context.method === "GET") {
        context.type = "html";
        context.body = details.prompt.name === "login"
          ? '<form method="post"><label>Provider password <input name="password" type="password"></label><button>Sign in to provider</button></form>'
          : '<form method="post"><button>Authorize provider</button></form>';
        return;
      }
      if (details.prompt.name === "login") {
        let body = "";
        for await (const chunk of context.req) body += chunk.toString();
        if (new URLSearchParams(body).get("password") !== "oidc-password") {
          context.status = 401;
          return;
        }
        await provider.interactionFinished(context.req, context.res, {
          login: { accountId: "oidc-user" },
        }, { mergeWithLastSubmission: false });
      } else {
        const grant = new provider.Grant({
          accountId: "oidc-user",
          clientId: String(details.params.client_id),
        });
        grant.addOIDCScope("openid profile email");
        await provider.interactionFinished(context.req, context.res, {
          consent: { grantId: await grant.save() },
        }, { mergeWithLastSubmission: true });
      }
      context.respond = false;
    });
    const metadata = {
      redirect_uris: ["http://127.0.0.1/callback"],
      token_endpoint_auth_method: "client_secret_basic",
      response_types: ["code"],
      grant_types: ["authorization_code"],
    };
    const registrationResponse = await fetch(`${issuer}/reg`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(metadata),
    });
    assertEquals(
      registrationResponse.status,
      201,
      await registrationResponse.clone().text(),
    );
    const registration: {
      client_id: string;
      client_secret: string;
      registration_client_uri: string;
      registration_access_token: string;
    } = await registrationResponse.json();
    await withTrellisRuntime(
      async (runtime) => {
        const updatedRegistration = await fetch(
          registration.registration_client_uri,
          {
            method: "PUT",
            headers: {
              "content-type": "application/json",
              authorization: `Bearer ${registration.registration_access_token}`,
            },
            body: JSON.stringify({
              ...metadata,
              client_id: registration.client_id,
              client_secret: registration.client_secret,
              redirect_uris: [`${runtime.trellisUrl}/auth/callback/oidc`],
            }),
          },
        );
        assertEquals(
          updatedRegistration.status,
          200,
          await updatedRegistration.text(),
        );
        await runtime.ensureAdmin();
        const settings = await runtime.callAdminRpc(
          "authPortalsLoginSettingsGet",
          { portalId: "builtin" },
        );
        await runtime.callAdminRpc("authPortalsLoginSettingsUpdate", {
          portalId: "builtin",
          expectedVersion: settings.version,
          idempotencyKey: ulid(),
          settings: {
            ...settings.settings,
            federatedRegistration: true,
            providers: ["local", "oidc"],
          },
        });
        const browser = await launchProfile(runtime);
        try {
          const page = await browser.newPage();
          await page.goto(`${runtime.trellisUrl}/console`, {
            waitUntil: "domcontentloaded",
          });
          await page.waitForURL((url) =>
            url.pathname === "/login" && url.searchParams.has("intent")
          );
          const startResponse = page.waitForResponse((response) =>
            new URL(response.url()).pathname === "/auth/login/oidc"
          );
          await page.getByRole("button", { name: /oidc/i }).click();
          const started = await startResponse;
          assertEquals(started.status(), 200, started.url());
          await page.getByLabel("Provider password").fill("oidc-password");
          await page.getByRole("button", { name: "Sign in to provider" })
            .click();
          const callbackCookies = await browser.cookies(
            `${runtime.trellisUrl}/auth/callback/oidc`,
          );
          const callbackResponse = page.waitForResponse((response) =>
            new URL(response.url()).pathname === "/auth/callback/oidc"
          );
          await page.getByRole("button", { name: "Authorize provider" })
            .click();
          const callback = await callbackResponse;
          assertEquals(callback.status(), 307, callback.url());
          await page.waitForURL((url) =>
            url.origin === runtime.trellisUrl && url.pathname === "/login" &&
            url.searchParams.has("transactionId")
          );
          await page.getByRole("button", { name: "Approve", exact: true })
            .waitFor({ state: "visible" });
          const missingCookieReplay = await browser.request.get(
            callback.url(),
            { maxRedirects: 0 },
          );
          assertEquals(missingCookieReplay.status(), 400);
          assertEquals(
            (await missingCookieReplay.json()).error.code,
            "oauth_browser_binding_invalid",
          );
          // A supported callback retry carries its original browser-continuity
          // credential. Completion cleared it; restore the real captured cookie.
          await browser.addCookies(callbackCookies);
          await page.goto(callback.url(), { waitUntil: "domcontentloaded" });
          await page.getByRole("button", { name: "Approve", exact: true })
            .click();
          await page.waitForURL((url) => url.pathname.startsWith("/console"));
          await waitForConsoleConnected(page);
        } finally {
          await browser.close();
        }
      },
      browserRuntimeOptions({
        oauthProviders: {
          oidc: {
            type: "oidc",
            issuer,
            clientId: registration.client_id,
            clientSecret: registration.client_secret,
          },
        },
      }),
    );
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve, reject) =>
      server.close((error) => error ? reject(error) : resolve())
    );
  }
});
