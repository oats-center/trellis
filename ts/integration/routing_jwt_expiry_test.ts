import { wsconnect } from "@nats-io/nats-core";
import { assertRejects } from "@std/assert";
import { fromFileUrl } from "@std/path";
import { createAuth } from "@oatscenter/trellis/auth";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { AuthorizationContextCache } from "../packages/trellis/auth/authorization/client_context.ts";
import { logger } from "../packages/trellis/globals.ts";
import { participantEvidence } from "../packages/trellis/participant_runtime/participant.ts";
import { fetchServiceBootstrapInfo } from "../packages/trellis/service/runtime/bootstrap.ts";
import { base64urlEncode } from "../packages/trellis/auth/utils.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test("an expired routing credential cannot admit a fresh NATS attachment", async () => {
  await withTrellisRuntime(async (runtime) => {
    const participant = participants.Provider.participant;
    const identity = await runtime.registerService({
      name: `expired-routing-${crypto.randomUUID()}`,
      contract: participant,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      seed: identity.seed,
    }).orThrow();
    try {
      const sessionAuth = await createAuth({
        sessionKeySeed: base64urlEncode(
          crypto.getRandomValues(new Uint8Array(32)),
        ),
      });
      const bootstrapRequest = {
        trellisUrl: runtime.trellisUrl,
        serviceName: "expired-routing",
        contractId: participant.identity,
        contractDigest: participantEvidence(participant).packageDigest,
        contract: participant,
        identityAuth: await createAuth({ sessionKeySeed: identity.seed }),
        sessionAuth,
        log: logger,
      };
      const bootstrap = await fetchServiceBootstrapInfo(bootstrapRequest);
      const contexts = new AuthorizationContextCache(runtime.trellisUrl);
      contexts.setServerClockOffsetMs(bootstrap.serverClockOffsetMs);
      const verified = await contexts.install(
        bootstrap.connectInfo.authorizationContext,
        {
          bootstrapJwt: bootstrap.connectInfo.jwt,
          bootstrapJwtExpiresAt: bootstrap.connectInfo.jwtExpiresAt,
        },
      );
      const options = await sessionAuth.natsConnectOptions({
        sessionId: bootstrap.connectInfo.connectionId,
        contextDigest: verified.contextDigest,
        jwt: bootstrap.connectInfo.jwt,
      });
      const servers = [runtime.natsWebsocketUrl];

      const admitted = await wsconnect({
        servers,
        authenticator: options.authenticator,
        maxReconnectAttempts: 0,
        timeout: 5_000,
      });
      await admitted.drain();

      // Exercise the exact original signed credential after its server-issued
      // admission deadline, without replacing it with a refreshed credential.
      await new Promise((resolve) =>
        setTimeout(
          resolve,
          Math.max(
            0,
            bootstrap.connectInfo.jwtExpiresAt * 1000 - Date.now() + 1100,
          ),
        )
      );
      const refreshed = await fetchServiceBootstrapInfo({
        ...bootstrapRequest,
        connectionId: bootstrap.connectInfo.connectionId,
      });
      const current = await contexts.install(
        refreshed.connectInfo.authorizationContext,
        {
          bootstrapJwt: refreshed.connectInfo.jwt,
          bootstrapJwtExpiresAt: refreshed.connectInfo.jwtExpiresAt,
        },
      );
      const currentOptions = await sessionAuth.natsConnectOptions({
        sessionId: bootstrap.connectInfo.connectionId,
        contextDigest: current.contextDigest,
        jwt: refreshed.connectInfo.jwt,
      });
      const currentConnection = await wsconnect({
        servers,
        authenticator: currentOptions.authenticator,
        maxReconnectAttempts: 0,
        timeout: 5_000,
      });
      await currentConnection.drain();
      const expiredOptions = await sessionAuth.natsConnectOptions({
        sessionId: bootstrap.connectInfo.connectionId,
        contextDigest: current.contextDigest,
        jwt: bootstrap.connectInfo.jwt,
      });
      await assertRejects(() =>
        wsconnect({
          servers,
          authenticator: expiredOptions.authenticator,
          maxReconnectAttempts: 0,
          timeout: 5_000,
        })
      );
    } finally {
      await service.stop();
    }
  }, {
    authorization: {
      contextLifetimeSeconds: 76,
      refreshLeadSeconds: 15,
      refreshJitterSeconds: 0,
      minimumContextLifetimeSeconds: 46,
    },
    trellis: {
      command: {
        cmd: Deno.env.get("TRELLIS_TEST_SERVER_BIN") ??
          fromFileUrl(
            new URL("../../target/debug/trellis-server", import.meta.url),
          ),
        args: ["--config", "{config}", "all"],
      },
    },
  });
});
