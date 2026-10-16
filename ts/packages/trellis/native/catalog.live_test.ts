import { ok } from "@oatscenter/result";
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { resolve } from "node:path";
import { test } from "node:test";
import { withTrellisRuntime } from "../../../integration/_support/runtime.ts";
import { pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { ulid } from "ulid";

import {
  apps,
  participants,
} from "../../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { loginNativeTestClient } from "../../trellis-testkit/src/admin/native_auth.ts";
import { generateSessionSeed } from "../../trellis-testkit/src/auth/random.ts";
import {
  apps as adminApps,
  types,
} from "../../trellis-testkit/trellis/index.js";
import { connectNativeClient } from "../native.ts";
import { createAuth } from "../portable.ts";
import { TrellisService } from "../service/mod.ts";

const exec = promisify(execFile);

test(
  "catalog expansion needs client renewal; forced incompatible replacement fences a running provider",
  { timeout: 120_000 },
  async (context) => {
    await withTrellisRuntime(async (runtime) => {
      const issuer = runtime.oauthIssuer;
      const trellisUrl = runtime.oauthIssuer;
      const username = runtime.adminUsername;
      const password = runtime.adminPassword;

      const directory = await mkdtemp("/tmp/opencode/native-catalog-");
      const close: Array<() => Promise<unknown>> = [];
      try {
        // Compile native IDL with the ordinary installed compiler in isolated output
        // directories. No shared generated SDK or hand-built wire schema is edited.
        await symlink(
          resolve("node_modules"),
          `${directory}/node_modules`,
          "dir",
        );
        const fixture = await readFile(
          "integration/fixtures/runtime/contract.trellis",
          "utf8",
        );
        const original =
          fixture.slice(
            fixture.indexOf("model Value"),
            fixture.indexOf("api events@v1"),
          ) + "\nservice EchoProvider { implements runtime; }\n";
        const baseline = original.replace(
          "consent_revision 1; allows { rpc Echo; }",
          "consent_revision 1; allows { rpc Download; }",
        );
        const expanded = baseline.replace(
          "consent_revision 1; allows { rpc Download; }",
          "consent_revision 1; allows { rpc Download; rpc Echo; }",
        );
        const incompatible = expanded.replace(
          "rpc Echo { input Value; output Value; }",
          "rpc Echo { input Value; output Empty; }",
        );
        const evidence: types.AuthPackageEvidence[] = [];
        for (const [index, source] of [
          baseline,
          expanded,
          incompatible,
        ].entries()) {
          const root = `${directory}/definition-${index}`;
          await mkdir(root);
          await writeFile(`${root}/contract.trellis`, source);
          await writeFile(
            `${root}/trellis.toml`,
            `[package]\nname = "runtime-trellis"\nversion = "0.100.0"\n[sources]\nruntime = "contract.trellis"\n[generate.typescript]\noutput = "trellis"\n`,
          );
          await exec(
            process.env.TRELLIS_TEST_CLI_BIN ?? "target/release/trellis",
            ["update", "--root", root],
          );
          const compiled = await exec(process.execPath, [
            "--experimental-transform-types",
            "--input-type=module",
            "-e",
            `import {participants} from ${JSON.stringify(pathToFileURL(`${root}/trellis/index.js`).href)}; console.log(JSON.stringify(participants.EchoProvider.participant.packageEvidence));`,
          ]);
          evidence.push(
            types.AuthPackageEvidenceCodec.decode(JSON.parse(compiled.stdout)),
          );
        }
        const adminLogin = await loginNativeTestClient({
          issuer,
          username,
          password,
          directory,
          clientId: "trellis.cli",
          request: adminApps.cli.request,
          platformPrivileges: {
            required: [
              "principals.manage",
              "roles.manage",
              "clients.manage",
              "apis.accept",
              "apis.forceReplace",
            ],
          },
          signal: context.signal,
        });
        const admin = await connectNativeClient({
          ...adminLogin,
          request: adminApps.cli.request,
          signal: context.signal,
        }).orThrow();
        close.push(() => admin.connection.close());
        const provider = participants.EchoProvider.participant;
        const apiId = provider.implements[0].identity;
        const { review: initialReview } = await admin
          .apisReview({ apiId, packageEvidence: evidence[0] })
          .orThrow();
        const initial = await admin
          .apisAccept({
            reviewId: initialReview.reviewId,
            canonicalDigest: initialReview.canonicalDigest,
            expectedGeneration: initialReview.currentGeneration ?? 0n,
            expectedAcceptedRevision:
              initialReview.currentAcceptedRevision ?? 0n,
            mappingEdits: [],
            roleEdits: [],
            idempotencyKey: ulid(),
          })
          .orThrow();
        const { deployment } = await admin
          .deploymentsCreate({
            title: "Catalog renewal provider",
            kind: provider.kind,
            participantId: provider.identity,
            packageEvidence: types.AuthPackageEvidenceCodec.decode(
              provider.packageEvidence,
            ),
            idempotencyKey: ulid(),
          })
          .orThrow();
        const seed = generateSessionSeed();
        const identity = await createAuth({ sessionKeySeed: seed });
        await admin
          .serviceInstancesProvision({
            deploymentId: deployment.deploymentId,
            identityPublicKey: identity.sessionKey,
            title: "Catalog renewal provider",
            idempotencyKey: ulid(),
          })
          .orThrow();
        const service = await TrellisService.connect({
          trellisUrl,
          participant: provider,
          seed,
        }).orThrow();
        close.push(() => service.stop());
        void service.wait().catch(() => undefined);
        await service.handleEcho(({ input }) => ok(input));
        const userPassword = `catalog-${ulid()}`;
        const { user } = await admin
          .usersCreate({
            username: `catalog-${ulid()}`,
            password: userPassword,
            displayName: "Catalog caller",
            idempotencyKey: ulid(),
          })
          .orThrow();
        const capabilityId = "runtime-trellis.runtime@v1::echo";
        await admin
          .capabilityGrantsGrant({
            principalId: user.principal.principalId,
            capabilityId,
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const clientId = `catalog-${ulid()}`;
        await admin
          .oAuthClientsPut({
            clientId,
            title: "Catalog caller",
            description: "Finite catalog membership renewal",
            kind: "native",
            redirectUris: [],
            development: true,
            impliedConsent: [],
            expectedRevision: 0n,
            idempotencyKey: ulid(),
          })
          .orThrow();
        const request = apps.EchoCaller.request;
        const login = await loginNativeTestClient({
          issuer,
          username: user.username,
          password: userPassword,
          directory,
          clientId,
          request,
          signal: context.signal,
        });
        const caller = await connectNativeClient({
          ...login,
          request,
          timeout: 2_000,
          signal: context.signal,
        }).orThrow();
        close.push(() => caller.connection.close());
        assert.equal(
          (await caller.echo({ value: "not yet granted" })).isErr(),
          true,
        );
        const { review } = await admin
          .apisReview({ apiId, packageEvidence: evidence[1] })
          .orThrow();
        assert.equal(review.compatible, true);
        assert.ok(
          review.currentGeneration !== undefined &&
            review.currentAcceptedRevision !== undefined,
        );
        const accepted = await admin
          .apisAccept({
            reviewId: review.reviewId,
            canonicalDigest: review.canonicalDigest,
            expectedGeneration: review.currentGeneration,
            expectedAcceptedRevision: review.currentAcceptedRevision,
            mappingEdits: [],
            roleEdits: [],
            idempotencyKey: ulid(),
          })
          .orThrow();
        assert.equal(accepted.api.generation, initial.api.generation);
        assert.equal(
          (await caller.echo({ value: "old authority" })).isErr(),
          true,
          "older signed membership must not inherit the new action",
        );
        await caller.refreshAuthority();
        assert.deepEqual(
          await caller.echo({ value: "renewed membership" }).orThrow(),
          { value: "renewed membership" },
        );
        const { review: replacement } = await admin
          .apisReview({ apiId, packageEvidence: evidence[2] })
          .orThrow();
        assert.equal(replacement.compatible, false);
        assert.ok(
          replacement.currentGeneration !== undefined &&
            replacement.currentAcceptedRevision !== undefined,
        );
        const forced = await admin
          .apisForceReplace({
            reviewId: replacement.reviewId,
            canonicalDigest: replacement.canonicalDigest,
            expectedGeneration: replacement.currentGeneration,
            expectedAcceptedRevision: replacement.currentAcceptedRevision,
            mappingEdits: [],
            roleEdits: [],
            acknowledgeBreakage: true,
            acknowledgeConsentImpact: true,
            idempotencyKey: ulid(),
          })
          .orThrow();
        assert.equal(forced.api.generation, accepted.api.generation + 1n);
        // Renewal must observe the new generation; existing provider authority
        // cannot answer as a provider for that generation.
        await caller.refreshAuthority().catch(() => undefined);
        assert.equal(
          (await caller.echo({ value: "old provider" })).isErr(),
          true,
        );
        assert.equal(
          (
            await TrellisService.connect({
              trellisUrl,
              participant: provider,
              seed,
            })
          ).isErr(),
          true,
          "a fenced deployment cannot reacquire a current provider certificate",
        );
      } finally {
        for (const stop of close.reverse()) await stop();
        await rm(directory, { recursive: true, force: true });
      }
    });
  },
);
