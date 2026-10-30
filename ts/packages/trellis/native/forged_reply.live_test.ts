import { ok } from "@oatscenter/result";
import assert from "node:assert/strict";
import {
  connect as connectTcp,
  createServer,
  type Server,
  type Socket,
} from "node:net";
import { once } from "node:events";
import { test } from "node:test";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { generateSessionSeed } from "../../trellis-testkit/src/auth/random.ts";
import {
  type NativeProxyTls,
  upgradeNatsTls,
} from "../../trellis-testkit/src/runtime.ts";
import {
  NatsFrameParser,
  type NatsOp,
} from "../../trellis-testkit/src/nats_wire.ts";
import { createResponseProof, type SessionResponse } from "../auth.ts";
import {
  apps as adminApps,
  types as adminTypes,
} from "../../trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../native.ts";
import { TransportError, createAuth } from "../portable.ts";
import { TrellisService } from "../service/mod.ts";

const echoCapability = "runtime-trellis.runtime.v1:echo.call";
type ReplyFrame = Extract<NatsOp, { kind: "msg" }>;
type ReplyMutation = (
  frame: ReplyFrame,
) => Promise<{ needle: Buffer; replacement: Buffer }>;

class CertifiedReplyProxy {
  readonly url: string;
  readonly #server: Server;
  readonly #sockets = new Set<Socket>();
  #armed?: {
    needle: Buffer;
    mutate: ReplyMutation;
    resolve: () => void;
  };

  readonly #broker: NativeProxyTls;

  private constructor(server: Server, target: URL, broker: NativeProxyTls) {
    this.#server = server;
    this.#broker = broker;
    const address = server.address();
    if (!address || typeof address === "string") {
      throw new Error("proxy did not bind a TCP port");
    }
    this.url = `tls://127.0.0.1:${address.port}`;
    server.on("connection", (client) => {
      void this.#forward(client, target).catch(() => client.destroy());
    });
  }

  /**
   * The attacker holds the broker's certificate, modelling a broken transport,
   * so the test proves certified replies resist tampering on their own.
   */
  static async start(
    targetUrl: string,
    broker: NativeProxyTls,
  ): Promise<CertifiedReplyProxy> {
    const target = new URL(targetUrl);
    const server = createServer();
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(0, "127.0.0.1", () => {
        server.off("error", reject);
        resolve();
      });
    });
    return new CertifiedReplyProxy(server, target, broker);
  }

  arm(value: string, mutate: ReplyMutation): Promise<void> {
    assert.equal(this.#armed, undefined, "a reply mutation is already armed");
    return new Promise((resolve) => {
      this.#armed = {
        needle: Buffer.from(JSON.stringify(value)),
        mutate,
        resolve,
      };
    });
  }

  async close(): Promise<void> {
    for (const socket of this.#sockets) socket.destroy();
    await new Promise<void>((resolve) => this.#server.close(() => resolve()));
  }

  async #forward(client: Socket, target: URL): Promise<void> {
    const plainUpstream = connectTcp({
      host: target.hostname,
      port: Number(target.port || 4222),
    });
    this.#sockets.add(client).add(plainUpstream);
    plainUpstream.on("error", () => plainUpstream.destroy());
    client.on("error", () => client.destroy());
    await once(plainUpstream, "connect");
    const [secureClient, upstream] = await upgradeNatsTls(
      client,
      plainUpstream,
      target.hostname,
      this.#broker,
      new AbortController().signal,
    );
    const close = () => {
      for (const socket of [secureClient, upstream, client, plainUpstream]) {
        socket.destroy();
        this.#sockets.delete(socket);
      }
    };
    secureClient.on("error", close).on("close", close).pipe(upstream);
    upstream.on("error", close).on("close", close);
    const parser = new NatsFrameParser();
    let forwarding = Promise.resolve();
    upstream.on("data", (chunk) => {
      for (const frame of parser.push(chunk)) {
        forwarding = forwarding
          .then(async () => {
            const armed = this.#armed;
            if (
              armed &&
              frame.op.kind === "msg" &&
              frame.op.headers?.["trellis-session-response"]
            ) {
              if (Buffer.from(frame.op.body).includes(armed.needle)) {
                const mutation = await armed.mutate(frame.op);
                assert.equal(
                  mutation.needle.length,
                  mutation.replacement.length,
                  "preserve the real HMSG frame lengths",
                );
                const raw = Buffer.from(frame.raw);
                const index = raw.indexOf(mutation.needle);
                assert.ok(
                  index >= 0,
                  "mutation must address bytes in the real certified reply",
                );
                mutation.replacement.copy(raw, index);
                this.#armed = undefined;
                armed.resolve();
                secureClient.write(raw);
                return;
              }
            }
            secureClient.write(frame.raw);
          })
          .catch((error: unknown) => {
            secureClient.destroy(
              error instanceof Error ? error : new Error(String(error)),
            );
          });
      }
    });
  }
}

test(
  "native caller rejects body, request-correlation and alternate-key certified reply attacks",
  { timeout: 45_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.trellisUrl;
      const trellisUrl = runtime.trellisUrl;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;

      const close: Array<() => Promise<unknown>> = [];
      try {
        const adminLogin = await loginNativeTestClient({
          issuer,
          username,
          password,
          clientId: "trellis.cli",
          request: adminApps.cli.request,
          administrativeCapabilities: [
            "trellis.auth.v1:accounts.manage",
            "trellis.auth.v1:users.manage",
            "trellis.auth.v1:apis.accept",
            "trellis.auth.v1:clients.manage",
            "trellis.auth.v1:roles.manage",
          ],
          signal: context.signal,
        });
        const admin = await connectNativeClient({
          ...adminLogin,
          request: adminApps.cli.request,
          signal: context.signal,
        }).orThrow();
        close.push(() => admin.connection.close());

        const provider = participants.EchoProvider.participant;
        const evidence = adminTypes.AuthPackageEvidenceCodec.decode(
          provider.packageEvidence,
        );
        const api = provider.implements[0];
        assert.ok(api, "EchoProvider must implement the runtime API");
        const { review } = await admin
          .apisReview({
            apiId: api.identity,
            packageEvidence: evidence,
          })
          .orThrow();
        assert.equal(review.compatible, true);
        await admin
          .apisAccept({
            reviewId: review.reviewId,
            canonicalDigest: review.canonicalDigest,
            expectedGeneration: review.currentGeneration ?? 0n,
            expectedAcceptedRevision: review.currentAcceptedRevision ?? 0n,
            mappingEdits: [],
            roleEdits: [],
            idempotencyKey: ulid(),
          })
          .orThrow();
        const { deployment } = await admin
          .deploymentsCreate({
            title: `forged-reply-${ulid()}`,
            kind: provider.kind,
            participantId: provider.identity,
            packageEvidence: evidence,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const seed = generateSessionSeed();
        const providerAuth = await createAuth({ sessionKeySeed: seed });
        await admin
          .serviceInstancesProvision({
            deploymentId: deployment.deploymentId,
            identityPublicKey: providerAuth.sessionKey,
            title: "forged reply provider",
            idempotencyKey: ulid(),
          })
          .orThrow();
        const service = await TrellisService.connect({
          trellisUrl,
          participant: provider,
          name: `forged-reply-provider-${ulid()}`,
          seed,
        }).orThrow();
        close.push(() => service.stop());
        await service.handleEcho(({ input }) => ok(input));

        const clientId = `forged-reply-${ulid()}`;
        const subjectPassword = `native-password-${ulid()}`;
        const { user } = await admin
          .usersCreate({
            username: `forged-reply-${ulid()}`,
            password: subjectPassword,
            displayName: "Forged reply subject",
            idempotencyKey: ulid(),
          })
          .orThrow();
        assert(user.account);
        await admin
          .capabilityGrantsGrant({
            principalId: user.principalId,
            capabilityId: echoCapability,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();

        const request = apps.EchoCaller.request;
        let login:
          Awaited<ReturnType<typeof loginNativeTestClient>> | undefined;
        await context.test(
          "a registered callback-free native client completes Device Code authorization",
          async () => {
            await admin
              .oAuthClientsPut({
                clientId,
                title: "Forged reply acceptance",
                description: "Native signed response verification acceptance.",
                kind: "native",
                redirectUris: [],
                impliedConsent: [],
                development: true,
                expectedRevision: 0n,
                idempotencyKey: ulid(),
              })
              .orThrow();
            login = await loginNativeTestClient({
              issuer,
              username: user.account.username,
              password: subjectPassword,
              clientId,
              request,
              approvedCapabilities: [echoCapability],
              signal: context.signal,
            });
          },
        );
        assert.ok(login, "native Device Code authorization must complete");
        const target = login.servers.find((server) =>
          server.startsWith("tls://"),
        );
        assert.ok(target, "runtime must advertise a native NATS endpoint");
        const proxy = await CertifiedReplyProxy.start(
          target,
          runtime.brokerTls(),
        );
        close.push(() => proxy.close());
        const caller = await connectNativeClient({
          ...login,
          servers: [proxy.url],
          request,
          timeout: 2_000,
          signal: context.signal,
        }).orThrow();
        close.push(() => caller.connection.close());

        assert.deepEqual(await caller.echo({ value: "baseline" }).orThrow(), {
          value: "baseline",
        });
        const alternateAuth = await createAuth({
          sessionKeySeed: generateSessionSeed(),
        });
        const attacks: Array<{ name: string; mutate: ReplyMutation }> = [
          {
            name: "body tampering",
            mutate: async () => ({
              needle: Buffer.from(JSON.stringify("forged")),
              replacement: Buffer.from(JSON.stringify("hacked")),
            }),
          },
          {
            name: "wrong request correlation",
            mutate: async (frame) => {
              assert.ok(frame.headers);
              const response: SessionResponse = JSON.parse(
                frame.headers["trellis-session-response"],
              );
              return {
                needle: Buffer.from(
                  `"requestId":${JSON.stringify(response.requestId)}`,
                ),
                replacement: Buffer.from(
                  `"requestId":${JSON.stringify(ulid())}`,
                ),
              };
            },
          },
          {
            name: "proof signed by an alternate runtime key",
            mutate: async (frame) => {
              assert.ok(frame.headers);
              const response: SessionResponse = JSON.parse(
                frame.headers["trellis-session-response"],
              );
              const proof = await createResponseProof(
                {
                  publicKey: alternateAuth.sessionKey,
                  sign: alternateAuth.sign,
                },
                response,
                frame.body,
              );
              return {
                needle: Buffer.from(frame.headers.proof),
                replacement: Buffer.from(proof),
              };
            },
          },
        ];
        for (const attack of attacks) {
          await context.test(attack.name, async () => {
            const mutated = proxy.arm("forged", attack.mutate);
            const rejected = caller.echo({ value: "forged" });
            await mutated;
            const result = await rejected;
            assert.equal(result.isErr(), true);
            if (result.isErr())
              assert.ok(result.error instanceof TransportError);
            assert.deepEqual(await caller.echo({ value: "after" }).orThrow(), {
              value: "after",
            });
          });
        }
      } finally {
        for (const dispose of close.reverse()) await dispose();
      }
    });
  },
);
