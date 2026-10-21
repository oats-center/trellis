import { connect } from "@nats-io/transport-node";
import assert from "node:assert/strict";
import { test } from "node:test";
import { connectNativeSession } from "@oatscenter/trellis/native";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { verifyOAuthAdmissionWasm } from "../packages/trellis/auth/protocol_wasm.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("expired OAuth admission cannot open a fresh NATS attachment", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      await runtime.registerService({
        name: "admission-provider",
        contract: participants.Provider.participant,
      });
      const login = await runtime.registerClient({
        name: "admission-expiry",
        contract: apps.EchoCaller.request,
      });
      const connected = await connectNativeSession(login).orThrow();
      const { session, servers } = login;
      const admission = session.admission;
      const verified = await verifyOAuthAdmissionWasm(
        new TextEncoder().encode(admission),
        {
          issuer: session.trust.issuer,
          trellisInstanceId: session.trust.instanceId,
          audienceNatsAccount: session.trust.account,
          ephemeralNkey: session.auth.sessionNkey,
          nowUnixSeconds: session.auth.currentIat(),
          maximumLifetimeSeconds: 300,
        },
      );
      assert(verified.ok);
      assert(
        verified.admission.credential.expiresAt -
          verified.admission.credential.issuedAt <= 18,
        "the isolated runtime must issue the configured short-lived admission",
      );
      try {
        const original = await session.auth.natsConnectOptions({
          inboxPrefix: connected.inboxPrefix,
          admission,
        });
        const before = await connect({
          servers: [...servers],
          authenticator: original.authenticator,
          maxReconnectAttempts: 0,
          timeout: 5000,
        });
        await before.close();
        const deadline = verified.admission.credential.expiresAt;
        await runtime.waitFor(() => Date.now() > (deadline + 1) * 1000, {
          timeoutMs: Math.max(0, deadline * 1000 - Date.now()) + 5000,
        });
        const renewed = (await session.refresh({})).orThrow();
        const current = await session.auth.natsConnectOptions({
          inboxPrefix: connected.inboxPrefix,
          admission: renewed.admission,
        });
        const fresh = await connect({
          servers: [...servers],
          authenticator: current.authenticator,
          maxReconnectAttempts: 0,
          timeout: 5000,
        });
        await fresh.close();
        const expired = await session.auth.natsConnectOptions({
          inboxPrefix: connected.inboxPrefix,
          admission,
        });
        await assert.rejects(() =>
          connect({
            servers: [...servers],
            authenticator: expired.authenticator,
            maxReconnectAttempts: 0,
            timeout: 5000,
          }),
        );
      } finally {
        await connected.manager.close();
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 90,
        refreshLeadSeconds: 15,
        refreshJitterSeconds: 5,
        minimumContextLifetimeSeconds: 40,
        oauthAdmissionLifetimeSeconds: 18,
      },
    },
  );
});
