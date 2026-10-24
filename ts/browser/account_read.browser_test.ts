import assert from "node:assert/strict";
import { test } from "node:test";
import { ulid } from "ulid";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
} from "@oatscenter/trellis/native";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  browserRuntimeOptions,
  launchProfile,
} from "./browser_test_support.ts";

test(
  "accountRead alone requires real consent and discloses only the consenting user's full self-profile",
  { timeout: 180_000 },
  async (t) => {
    await withTrellisRuntime(async (runtime) => {
      const password = "account-profile-consent-password";
      const { user } = await runtime.callAdminRpc("authUsersCreate", {
        username: "self-profile-reader",
        password,
        displayName: "Profile reader",
        givenName: "Profile",
        familyName: "Reader",
        email: "reader@example.com",
        idempotencyKey: ulid(),
      });
      const { user: other } = await runtime.callAdminRpc("authUsersCreate", {
        username: "other-profile",
        displayName: "Other account",
        idempotencyKey: ulid(),
      });
      const request = apps.AccountCaller.request;
      await runtime.callAdminRpc("authOAuthClientsPut", {
        clientId: request.id,
        kind: "native",
        title: "Self-profile reader",
        description: "Read only the consenting account",
        redirectUris: ["http://127.0.0.1/callback"],
        development: true,
        impliedConsent: [],
        expectedRevision: 0n,
        idempotencyKey: ulid(),
      });
      const server = (await discoverNativeOAuth(runtime.oauthIssuer)).orThrow();
      const custody = (
        await NativeOAuthClient.open({
          scope: {
            issuer: server.issuer,
            instanceId: server.trust.instanceId,
            account: server.trust.account,
            clientId: request.id,
            credentialHandle: ulid(),
          },
          trust: server.trust,
        })
      ).orThrow();
      const browser = await launchProfile(runtime);
      browser.setDefaultTimeout(30_000);
      let phase = "open browser";
      const page = await browser.newPage();
      try {
        for (const decision of ["unavailable", "denied", "approved"]) {
          phase = `${decision}: start device`;
          const flow = (
            await custody.startDevice(request, { signal: t.signal })
          ).orThrow();
          await page.goto(flow.verificationUri);
          await page.getByLabel("Code", { exact: true }).fill(flow.userCode);
          await page
            .getByRole("button", { name: "Continue", exact: true })
            .click();
          if (decision === "unavailable") {
            phase = `${decision}: sign in`;
            await page
              .getByLabel("Username", { exact: true })
              .fill("self-profile-reader");
            await page.getByLabel("Password", { exact: true }).fill(password);
            await page
              .getByRole("button", { name: "Sign in", exact: true })
              .click();
          }
          phase = `${decision}: present consent`;
          await page
            .getByRole("heading", { name: /wants to use your account/ })
            .waitFor();
          const approve = page.getByRole("button", {
            name: "Allow",
            exact: true,
          });
          assert.equal(await approve.isDisabled(), decision === "unavailable");
          if (decision === "approved") {
            await page.locator('input[name="confirm_device"]').check();
            await page.screenshot({
              path: "target/review-account-consent.png",
              fullPage: true,
            });
            await approve.click();
            phase = `${decision}: exchange credential`;
            const session = (
              await flow.complete({
                signal: AbortSignal.any([
                  t.signal,
                  AbortSignal.timeout(20_000),
                ]),
              })
            ).orThrow();
            const caller = await connectNativeClient({
              session,
              servers: server.natsServers,
              request,
            }).orThrow();
            try {
              const own = (await caller.usersResolve({}).orThrow()).user;
              assert.equal(own.principalId, user.principalId);
              assert.equal(own.displayName, user.displayName);
              assert.equal(own.givenName, user.givenName);
              assert.equal(own.familyName, user.familyName);
              assert.equal(own.email, user.email);
              assert.equal(own.emailVerified, false);
              assert.equal(own.account?.username, "self-profile-reader");
              const stillOwn = (
                await caller
                  .usersResolve({ userId: other.principalId })
                  .orThrow()
              ).user;
              assert.equal(stillOwn.principalId, user.principalId);
              assert.notEqual(stillOwn.principalId, other.principalId);
            } finally {
              await caller.connection.close();
            }
          } else {
            await page
              .getByRole("button", { name: "Cancel", exact: true })
              .click();
            phase = `${decision}: observe denied device result`;
            const denied = await flow.complete({
              signal: AbortSignal.any([t.signal, AbortSignal.timeout(20_000)]),
            });
            assert(denied.isErr());
            assert.equal(denied.error.reason, "OAuthAuthorizationDenied");
            if (decision === "unavailable")
              await runtime.callAdminRpc("authCapabilityGrantsGrant", {
                principalId: user.principalId,
                capabilityId: "trellis.auth.v1:account.read",
                expectedRevision: 0n,
                idempotencyKey: ulid(),
              });
          }
        }
      } catch (error) {
        t.diagnostic(`${phase}: ${await page.locator("body").innerText()}`);
        await page.screenshot({
          path: "target/review-account-failure.png",
          fullPage: true,
        });
        throw error;
      } finally {
        await browser.close();
        await custody.close();
      }
    }, browserRuntimeOptions());
  },
);
