import assert from "node:assert/strict";
import { test } from "node:test";
import { join } from "node:path";
import type { ChildProcessWithoutNullStreams } from "node:child_process";
import { z } from "zod";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apps,
  participants,
  types,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { base64urlEncode } from "../packages/trellis/auth/utils.ts";
import { approveNativeDeviceCode } from "../packages/trellis-testkit/src/admin/native_auth.ts";
import { spawnTracked, type ProcessExit } from "./_support/process.ts";
import { rustFixtureArgv, withTrellisRuntime } from "./_support/runtime.ts";

test("Rust device companion renews admitted authority and resumes after restart without new consent", async () => {
  await withTrellisRuntime(
    async (runtime) => {
      const providerIdentity = await runtime.registerService({
        deployment: "rust-companion-provider",
        name: "rust-companion-provider",
        contract: participants.ChildProvider.participant,
      });
      const deployment = await runtime.contracts.apply({
        deployment: "rust-companion-device",
        contract: participants.AgentDevice.participant,
      });
      const { deploymentId } = types.AuthDeploymentGetRequestCodec.decode({
        deploymentId: deployment.deploymentId,
      });
      const { provisioningSecret } = await runtime.devices.provision({
        deploymentId,
        title: "Rust companion device",
        idempotencyKey: ulid(),
      });
      assert(provisioningSecret);
      const prepared = await runtime.registerClient({
        name: "rust-companion-eligibility",
        contract: apps.AgentDevice_Companion.request,
      });
      await prepared.session.close();
      const rootSecret = crypto.getRandomValues(new Uint8Array(32));
      let nonce: string | undefined;
      let scope: string | undefined;
      let clientId: string | undefined;
      for (let leg = 0; leg < 2; leg++) {
        if (leg === 1) await runtime.restart();
        const service = await TrellisService.connect({
          trellisUrl: runtime.oauthIssuer,
          participant: participants.ChildProvider.participant,
          seed: providerIdentity.seed,
        }).orThrow();
        const serviceExit = service.wait().catch((error: unknown) => error);
        let authorizationSessionId: string | undefined;
        await service.handleRequired(({ input, context: { caller } }) => {
          assert(caller.type === "verified");
          authorizationSessionId = caller.authorizationSessionId;
          return Result.ok(input);
        });
        await service.handleOptional(({ input }) => Result.ok(input));
        const [command, ...args] = rustFixtureArgv("device_companion");
        const child: ChildProcessWithoutNullStreams & {
          exited: Promise<ProcessExit>;
        } = spawnTracked(command, args, {
          env: {
            ...process.env,
            TRELLIS_URL: runtime.oauthIssuer,
            TRELLIS_CA_CERTIFICATE: runtime.caCertificate,
            DEVICE_ROOT_SECRET: base64urlEncode(rootSecret),
            XDG_CONFIG_HOME: join(runtime.workdir, "rust-companion-config"),
            ...(leg === 0
              ? { TRELLIS_PROVISIONING_SECRET: provisioningSecret }
              : {
                  DEVICE_ACTIVATION_NONCE: nonce,
                  COMPANION_CREDENTIAL_SCOPE: scope,
                }),
          },
        });
        let failure: unknown;
        let exited = false;
        let stderr = "";
        const lines: string[] = [];
        child.stderr.setEncoding("utf8").on("data", (chunk: string) => {
          stderr += chunk;
        });
        child.stdin.on("error", (error) => {
          failure ??= error;
        });
        void child.exited
          .then(() => {
            exited = true;
          })
          .catch((error: unknown) => {
            failure ??= error;
          });
        const drain = (async () => {
          let pending = "";
          for await (const chunk of child.stdout) {
            pending += chunk.toString();
            const complete = pending.split("\n");
            pending = complete.pop() ?? "";
            for (const raw of complete) {
              const line = raw.trim();
              lines.push(line);
              if (line.startsWith("DEVICE_COMPANION_ACTIVATION ")) {
                assert.equal(leg, 0, "restart must not require enrollment");
                const activation = z
                  .object({
                    reviewId: z.string(),
                    confirmationCode: z.string(),
                    nonce: z.string(),
                  })
                  .parse(
                    JSON.parse(
                      line.slice("DEVICE_COMPANION_ACTIVATION ".length),
                    ),
                  );
                nonce = activation.nonce;
                const { review } = await runtime.callAdminRpc(
                  "authDevicesReviewsGet",
                  types.AuthDeviceReviewGetRequestCodec.decode({
                    reviewId: activation.reviewId,
                    confirmationCode: activation.confirmationCode,
                  }),
                );
                assert(review.companionClientId);
                clientId = review.companionClientId;
                const approved = await runtime.callAdminRpc(
                  "authDevicesReviewsDecide",
                  {
                    reviewId: review.reviewId,
                    confirmationCode: activation.confirmationCode,
                    expectedRevision: review.revision,
                    decision: "approved",
                    idempotencyKey: ulid(),
                  },
                );
                child.stdin.write(
                  `${JSON.stringify(types.AuthDeviceReviewCodec.encode(approved.review))}\n`,
                );
              } else if (line.startsWith("NATIVE_LOGIN ")) {
                assert.equal(
                  leg,
                  0,
                  "restart must resume the existing native grant",
                );
                const login = z
                  .object({ clientId: z.string(), userCode: z.string() })
                  .parse(JSON.parse(line.slice("NATIVE_LOGIN ".length)));
                assert.equal(login.clientId, clientId);
                await approveNativeDeviceCode({
                  issuer: runtime.oauthIssuer,
                  clientId: login.clientId,
                  userCode: login.userCode,
                  username: runtime.adminUsername,
                  password: runtime.adminPassword,
                  approvedCapabilities:
                    apps.AgentDevice_Companion.request.requiredCapabilities,
                });
              } else if (line.startsWith("DEVICE_COMPANION_SCOPE ")) {
                scope = line.slice("DEVICE_COMPANION_SCOPE ".length);
              } else if (line.startsWith("DEVICE_COMPANION_ERROR ")) {
                throw new Error(line);
              }
            }
          }
        })();
        void drain.catch((error: unknown) => {
          failure ??= error;
        });
        const marker = async (expected: string) => {
          const observed = await runtime.waitFor(
            () => {
              if (failure) return { error: failure };
              if (lines.includes(expected)) return { error: undefined };
              if (exited)
                return {
                  error: new Error(
                    `Native companion exited before ${expected}: ${stderr}`,
                  ),
                };
              return false;
            },
            { timeoutMs: 45_000, intervalMs: 50 },
          );
          if (observed.error) throw observed.error;
        };
        try {
          await marker("DEVICE_COMPANION_READY");
          if (leg === 0) {
            assert(nonce);
            assert(scope);
            assert(clientId);
            const sessions = await runtime.callAdminRpc(
              "authAuthorizationSessionsList",
              { clientId, state: "active" },
            );
            const active = sessions.items.find(
              (session) =>
                session.authorizationSessionId === authorizationSessionId,
            );
            assert(active?.maximumContextExpiresAt);
            const previousExpiry = active.maximumContextExpiresAt;
            await runtime.waitFor(
              async () => {
                if (failure) throw failure;
                const current = await runtime.callAdminRpc(
                  "authAuthorizationSessionsList",
                  { clientId, state: "active" },
                );
                const same = current.items.find(
                  (session) =>
                    session.authorizationSessionId ===
                    active.authorizationSessionId,
                );
                return (
                  same?.maximumContextExpiresAt !== undefined &&
                  Date.parse(same.maximumContextExpiresAt) >
                    Date.parse(previousExpiry)
                );
              },
              {
                timeoutMs: Math.max(1, Date.parse(previousExpiry) - Date.now()),
                intervalMs: 500,
              },
            );
            child.stdin.write("REQUIRED\n");
            await marker("DEVICE_COMPANION_RENEWED_OK");
          }
          child.stdin.end("EXIT\n");
          await marker("DEVICE_COMPANION_DONE");
          assert.equal((await child.exited).code, 0, stderr);
          await drain;
        } finally {
          if (!exited) child.kill("SIGTERM");
          await child.exited;
          await service.stop();
          assert.equal(await serviceExit, undefined);
        }
      }
    },
    {
      authorization: {
        contextLifetimeSeconds: 53,
      },
    },
  );
});
