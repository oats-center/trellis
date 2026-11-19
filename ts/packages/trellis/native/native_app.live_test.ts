import assert from "node:assert/strict";
import { test } from "node:test";
import { isErr } from "@oatscenter/result";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { apps } from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { approveNativeDeviceCode } from "../../trellis-testkit/src/admin/native_auth.ts";
import type { JsonValue } from "../generated.ts";
import {
  connectNativeClient,
  discoverNativeOAuth,
  NativeOAuthClient,
} from "../native.ts";

test(
  "a native app request signs in with Device Code on a redirect-free native client; other kinds fail naming both",
  { timeout: 60_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const signal = context.signal;
      // `native app AccountCaller` requires one capability, account.read.
      const native = apps.AccountCaller.request;
      const browser = apps.StateCaller.request;
      const nativeClient = `native-app-${ulid()}`;
      const browserClient = `browser-app-${ulid()}`;
      for (const [clientId, kind, redirectUris] of [
        [nativeClient, "native", []],
        [browserClient, "browser", ["https://app.example.test/callback"]],
      ] as const)
        await runtime.callAdminRpc("authOAuthClientsPut", {
          clientId,
          kind,
          title: `${kind} app`,
          description: "Native app declaration acceptance",
          redirectUris: [...redirectUris],
          development: false,
          impliedConsent: [],
          expectedRevision: 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
      const username = `native-app-${ulid()}`;
      const password = `native-password-${ulid()}`;
      const { user } = await runtime.callAdminRpc("authUsersCreate", {
        username,
        password,
        displayName: "Native app user",
        idempotencyKey: codecs.ulid.decode(ulid()),
      });

      const server = (
        await discoverNativeOAuth(runtime.trellisUrl, { signal })
      ).orThrow();
      const open = async (clientId: string) =>
        (
          await NativeOAuthClient.open({
            scope: {
              issuer: server.issuer,
              instanceId: server.trust.instanceId,
              account: server.trust.account,
              clientId,
              credentialHandle: ulid(),
            },
            trust: server.trust,
            signal,
          })
        ).orThrow();

      const login = await open(nativeClient);
      // A browser request is refused before anything reaches Auth.
      const wrongRequest = await login.startDevice(browser, { signal });
      assert.equal(wrongRequest.error?.reason, "AppKindMismatch");
      assert.match(
        wrongRequest.error?.message ?? "",
        /native app request.*'runtime-trellis\.StateCaller' is a browser app request/,
      );

      const device = (await login.startDevice(native, { signal })).orThrow();
      await approveNativeDeviceCode({
        issuer: server.issuer,
        clientId: nativeClient,
        userCode: device.userCode,
        username,
        password,
        signal,
      });
      const session = (await device.complete({ signal })).orThrow();
      const options = {
        session,
        servers: server.natsServers,
        natsSentinel: server.natsSentinel,
        signal,
      };
      const refused = (
        await connectNativeClient({ ...options, request: browser })
      ).take();
      assert.ok(isErr(refused));
      assert.equal(refused.error.reason, "AppKindMismatch");
      assert.match(
        refused.error.message,
        /connectNativeClient needs a native app request.*is a browser app request/,
      );
      const client = await connectNativeClient({
        ...options,
        request: native,
      }).orThrow();
      try {
        const me = await client.sessionsMe({}).orThrow();
        assert.equal(me.principal.principalId, user.principalId);
      } finally {
        await client.connection.close();
      }

      // Auth attaches a native app's settings only to a native client.
      const configured = apps.ConfiguredAgent.request;
      const declare = (clientId: string) =>
        runtime.callAdminRpc("authConfigAppsDeclare", {
          clientId,
          packageEvidence: configured.packageEvidence as JsonValue,
          requestId: configured.id,
          idempotencyKey: codecs.ulid.decode(ulid()),
        });
      await declare(nativeClient);
      await assert.rejects(
        declare(browserClient),
        new RegExp(
          `runtime-trellis\\.ConfiguredAgent is declared as a native app, but app ${browserClient} is registered as a browser app`,
        ),
      );

      // Device Code against a browser registration names both kinds.
      const onBrowser = await open(browserClient);
      try {
        const mismatch = await onBrowser.startDevice(native, { signal });
        assert.equal(mismatch.error?.reason, "OAuthUnauthorizedClient");
        assert.match(
          mismatch.error?.message ?? "",
          new RegExp(
            `needs a native app client, but client '${browserClient}' is registered as a browser app`,
          ),
        );
      } finally {
        await onBrowser.close();
      }
    });
  },
);
