import assert from "node:assert/strict";
import { setTimeout } from "node:timers/promises";
import { test } from "node:test";
import { connect } from "@nats-io/transport-node";
import {
  createInbox,
  headers,
  PermissionViolationError,
} from "@nats-io/nats-core";
import type { SessionRequest } from "../auth.ts";
import { ulid } from "ulid";
import { apis, apps } from "../../trellis-testkit/trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { connectNativeClient, connectNativeSession } from "../native.ts";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";

const sessionsRead = "trellis.auth@v1::sessionsRead";
const usersCreate = "trellis.auth@v1::usersCreate";

test(
  "native signed RPCs, explicit consent deselection, same-attachment renewal and enforced policy reduction",
  { timeout: 45000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;
      const close: Array<() => Promise<unknown>> = [];
      const signal = context.signal;
      try {
        const adminLogin = await loginNativeTestClient({
          issuer,
          username,
          password,
          clientId: "trellis.cli",
          request: apps.cli.request,
          platformPrivileges: {
            required: ["principals.manage", "roles.manage"],
          },
          signal,
        });
        const admin = await connectNativeClient({
          ...adminLogin,
          request: apps.cli.request,
          signal,
        }).orThrow();
        close.push(() => admin.connection.close());
        const subjectUsername = `native-gate-${ulid()}`;
        const subjectPassword = `native-password-${ulid()}`;
        const subject = await admin
          .usersCreate({
            username: subjectUsername,
            password: subjectPassword,
            displayName: "Native gate subject",
            idempotencyKey: ulid(),
          })
          .orThrow();
        const approved = [
          ...new Set([
            ...apps.cli.request.requiredCapabilities,
            sessionsRead,
            usersCreate,
          ]),
        ];
        await Promise.all(
          approved.map((capabilityId) =>
            admin
              .capabilityGrantsGrant({
                principalId: subject.user.principal.principalId,
                capabilityId,
                expectedRevision: 0n,
                idempotencyKey: ulid(),
              })
              .orThrow(),
          ),
        );
        const credentials = {
          issuer,
          username: subjectUsername,
          password: subjectPassword,
          clientId: "trellis.cli",
          request: apps.cli.request,
          signal,
        };

        const ordinaryLogin = await loginNativeTestClient({
          ...credentials,
          approvedCapabilities: approved,
        });
        const ordinary = await connectNativeClient({
          ...ordinaryLogin,
          request: apps.cli.request,
          signal,
        }).orThrow();
        close.push(() => ordinary.connection.close());
        const sessions = await ordinary.sessionsList({}).orThrow();
        assert.ok(
          sessions.items.some(
            (item) =>
              item.loginSessionId === ordinaryLogin.session.loginSessionId &&
              item.principalId === subject.user.principal.principalId,
          ),
        );
        const denied = await ordinary.usersCreate({
          username: `must-not-exist-${ulid()}`,
          displayName: "Denied",
          idempotencyKey: ulid(),
        });
        assert.ok(denied.isErr());
        assert.ok(
          denied.error instanceof apis.auth.AuthError,
          "selected Users.Create must return a verified declared Auth failure, not a transport failure",
        );
        await ordinary.connection.close();

        const declinedLogin = await loginNativeTestClient({
          ...credentials,
          approvedCapabilities: approved.filter(
            (capability) => capability !== sessionsRead,
          ),
        });
        const declined = await connectNativeClient({
          ...declinedLogin,
          request: apps.cli.request,
          signal,
        }).orThrow();
        close.push(() => declined.connection.close());
        assert.equal(
          (await declined.sessionsList({})).isErr(),
          true,
          "consent omission must make the selected optional RPC unusable",
        );
        assert.ok(declined.ownAuthority.current);
        assert.equal(
          declined.ownAuthority.current.capabilities.some(
            (capability) => capability.capabilityId === sessionsRead,
          ),
          false,
        );
        await context.test(
          "a genuinely lesser authority cannot inject a valid signed raw NATS RPC",
          async () => {
            const owner = declinedLogin.session.auth.authorityOwner;
            assert.ok(owner);
            const lease = owner.own();
            const own = lease.authority;
            const api = own.apis.find(
              (entry) => entry.apiId === apis.auth.API.identity,
            );
            assert.ok(api);
            const descriptor = apis.auth.API.actions["rpc:Sessions.List"];
            const payload = new TextEncoder().encode(
              JSON.stringify(descriptor.input.encode({})),
            );
            const socket = await connect({
              servers: [...declinedLogin.servers],
              ...(await declinedLogin.session.auth.natsConnectOptions({
                admission: declinedLogin.session.admission,
                inboxPrefix: own.inboxPrefix,
              })),
              reconnect: false,
            });
            try {
              const reply = createInbox(own.inboxPrefix);
              const request: SessionRequest = {
                authorityDigest: lease.digest,
                apiId: api.apiId,
                apiGeneration: api.generation,
                acceptedRevision: api.acceptedRevision,
                action: "Sessions.List",
                subject: descriptor.subject,
                replySubject: reply,
                requestId: ulid(),
                issuedAt: declinedLogin.session.auth.currentIat(),
              };
              const proof = await declinedLogin.session.auth.createProof(
                request,
                payload,
              );
              const publication = headers();
              publication.set("authorization-context", request.authorityDigest);
              publication.set(
                "session-key",
                declinedLogin.session.auth.sessionKey,
              );
              publication.set("proof", proof.proof);
              publication.set("iat", String(request.issuedAt));
              publication.set("request-id", request.requestId);
              publication.set(
                "Trellis-Session-Request",
                JSON.stringify(request),
              );
              publication.set("Trellis-Api-Generation", request.apiGeneration);
              publication.set(
                "Trellis-Accepted-Revision",
                request.acceptedRevision,
              );
              const statuses = socket.status();
              const abort = () => {
                void socket.close();
              };
              signal.addEventListener("abort", abort, { once: true });
              try {
                const denied = (async () => {
                  for await (const status of statuses) {
                    if (
                      status.type === "error" &&
                      status.error instanceof PermissionViolationError
                    ) {
                      assert.equal(status.error.operation, "publish");
                      assert.equal(status.error.subject, descriptor.subject);
                      return;
                    }
                  }
                  assert.fail(
                    "broker closed without reporting the forbidden raw publication",
                  );
                })();
                socket.publish(descriptor.subject, payload, {
                  headers: publication,
                  reply,
                });
                await denied;
              } finally {
                signal.removeEventListener("abort", abort);
              }
            } finally {
              lease.release();
              await socket.close();
            }
          },
        );
        await declined.connection.close();

        const login = await loginNativeTestClient({
          ...credentials,
          approvedCapabilities: approved,
        });
        const session = login.session;
        const raw = await connectNativeSession({ ...login, signal }).orThrow();
        close.push(() => raw.close());
        // The second physical attachment has no SDK owner or revocation observer.
        // Its closure can only be broker enforcement, not cooperative SDK cleanup.
        const uncooperative = await connect({
          servers: [...login.servers],
          ...(await session.auth.natsConnectOptions({
            admission: session.admission,
            inboxPrefix: raw.inboxPrefix,
          })),
          maxReconnectAttempts: 0,
          reconnect: false,
        });
        close.push(() => uncooperative.close());
        const socket = raw.manager.currentNats();
        const generation = raw.manager.publishedGenerationId();
        const brokerClientId = socket.info?.client_id;
        assert.ok(brokerClientId);
        const before = raw.owner.own();
        const baseline = { ...before.authority, digest: before.digest };
        before.release();
        // The signed wire timestamps use whole seconds. Cross the actual issuance
        // boundary, rather than sleeping for an arbitrary readiness interval.
        await setTimeout(
          Math.max(0, (Math.floor(Date.now() / 1000) + 1) * 1000 - Date.now()),
          undefined,
          { signal },
        );
        await context.test(
          "concurrent renewal succeeds on the original physical attachment",
          async () => {
            await Promise.all([raw.refreshAuthority(), raw.refreshAuthority()]);
            assert.equal(raw.manager.currentNats(), socket);
            assert.equal(raw.manager.publishedGenerationId(), generation);
            await socket.flush();
          },
        );
        const retained = raw.owner.own();
        close.push(async () => retained.release());
        assert.ok(retained.authority.expiresAt > baseline.expiresAt);
        assert.notEqual(retained.digest, baseline.digest);
        assert.equal(
          retained.authority.authorizationSessionId,
          baseline.authorizationSessionId,
        );
        assert.equal(retained.authority.runtimeId, baseline.runtimeId);
        assert.equal(
          retained.authority.sessionPublicKey,
          baseline.sessionPublicKey,
        );
        assert.equal(raw.manager.currentNats(), socket);
        assert.equal(raw.manager.publishedGenerationId(), generation);
        assert.equal(socket.info?.client_id, brokerClientId);
        await socket.flush();

        const retiredId = retained.authority.authorizationSessionId;
        const grants = await admin
          .capabilityGrantsList({
            principalId: subject.user.principal.principalId,
            capabilityId: sessionsRead,
          })
          .orThrow();
        const grant = grants.items.find(
          (item) =>
            item.principalId === subject.user.principal.principalId &&
            item.capability.capabilityId === sessionsRead,
        );
        assert.ok(
          grant,
          "the target user's actual direct grant must exist before reduction",
        );
        let unsubscribe = () => {};
        const reduced = new Promise<void>((resolve, reject) => {
          const changed = () => {
            try {
              const current = raw.owner.own();
              try {
                if (current.authority.authorizationSessionId !== retiredId)
                  resolve();
              } finally {
                current.release();
              }
            } catch {
              /* A revoked attachment is unusable until a new admission is promoted. */
            }
          };
          unsubscribe = raw.owner.subscribeChanges(changed);
          signal.addEventListener("abort", () => reject(signal.reason), {
            once: true,
          });
        });
        const kicked = uncooperative.closed();
        try {
          await Promise.all([
            context.test(
              "targeted direct capability revocation succeeds",
              async () => {
                await admin
                  .capabilityGrantsRevoke({
                    grantId: grant.grantId,
                    expectedRevision: grant.revision,
                    idempotencyKey: ulid(),
                  })
                  .orThrow();
              },
            ),
            context.test(
              "broker closes an attachment with no cooperative owner",
              async () => {
                await kicked;
                assert.ok(uncooperative.isClosed());
              },
            ),
            context.test(
              "the SDK reconnects with reduced authority in a new logical session",
              async () => {
                await reduced;
                assert.ok(raw.owner.isRevoked(retiredId));
                assert.throws(() => retained.assertCurrent());
              },
            ),
          ]);
        } finally {
          unsubscribe();
        }
        assert.ok(uncooperative.isClosed());
        assert.ok(
          raw.owner.isRevoked(retiredId),
          "the shared owner must authenticate and retain the signed retirement",
        );
        assert.throws(() => retained.assertCurrent());
        const current = raw.owner.own();
        try {
          assert.notEqual(current.authority.authorizationSessionId, retiredId);
          assert.equal(current.authority.oauthGrantId, baseline.oauthGrantId);
          assert.equal(current.authority.runtimeId, baseline.runtimeId);
          assert.equal(
            current.authority.sessionPublicKey,
            baseline.sessionPublicKey,
          );
          assert.equal(
            current.authority.capabilities.some(
              (capability) => capability.capabilityId === sessionsRead,
            ),
            false,
          );
          assert.equal(
            await current.allows("trellis.auth@v1", {
              kind: "rpc",
              name: "Sessions.List",
              direction: "call",
            }),
            false,
          );
          assert.equal(
            await current.allows("trellis.auth@v1", {
              kind: "rpc",
              name: "UserIdentities.List",
              direction: "call",
            }),
            true,
          );
          assert.notEqual(
            raw.manager.currentNats().info?.client_id,
            brokerClientId,
          );
          await raw.manager.currentNats().flush();
        } finally {
          current.release();
        }
      } finally {
        for (const dispose of close.reverse()) await dispose();
      }
    });
  },
);
