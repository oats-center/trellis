/**
 * Live acceptance for showing a stored secret to an administrator: only a
 * role that manages secrets is shown one, through a deployment, an
 * instance, or a service claim; any other settings role is told what it
 * lacks; every reveal is recorded without its value; and reveals are
 * rate-limited per administrator.
 */

import { TrellisService } from "@oatscenter/trellis/service";
import { codecs } from "@oatscenter/trellis/generated";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test } from "node:test";
import nacl from "tweetnacl";
import { ulid } from "ulid";

import {
  apis,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test("a secret is shown only to a role that manages secrets, and every reveal is recorded without its value", async () => {
  await withTrellisRuntime(async (runtime) => {
    const key = await runtime.services.createInstance({
      name: "revealed",
      deployment: "revealed",
      contract: participants.ConfiguredService.participant,
      config: {
        values: { endpoint: "https://one.example" },
        secrets: { apiKey: "deployment-key" },
      },
    });
    const deployment = { deploymentId: codecs.ulid.decode(key.deploymentId) };
    const instance = {
      ...deployment,
      instanceId: codecs.ulid.decode(key.instanceId),
    };
    const { claim } = await runtime.callAdminRpc("authServiceClaimsCreate", {
      title: "revealed-claim",
      instanceTitle: "revealed-claim",
      identityPublicKey: Buffer.from(
        nacl.sign.keyPair.fromSeed(randomBytes(32)).publicKey,
      ).toString("base64url"),
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    await runtime.callAdminRpc("authConfigSet", {
      target: { claimId: claim.claimId },
      secrets: { apiKey: "claim-key" },
      idempotencyKey: codecs.ulid.decode(ulid()),
    });

    const connect = async (
      name: string,
      contract:
        | typeof participants.ConfigKeeper.participant
        | typeof participants.ConfigTuner.participant,
    ) => {
      const identity = await runtime.registerService({ name, contract });
      return await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: contract,
        seed: identity.seed,
      }).orThrow();
    };
    const keeper = await connect(
      "secret-keeper",
      participants.ConfigKeeper.participant,
    );
    const tuner = await connect(
      "secret-tuner",
      participants.ConfigTuner.participant,
    );
    try {
      const reveal = (
        target: Parameters<typeof keeper.configSecretsReveal>[0]["target"],
      ) => keeper.configSecretsReveal({ target, field: "apiKey" }).orThrow();

      // The deployment's secret shows through it and through an instance
      // without overrides; an instance's own override shows as its own.
      assert.deepEqual(await reveal(deployment), {
        value: "deployment-key",
        source: "deployment",
      });
      assert.deepEqual(await reveal(instance), {
        value: "deployment-key",
        source: "deployment",
      });
      await runtime.callAdminRpc("authConfigSet", {
        target: instance,
        secrets: { apiKey: "instance-key" },
        idempotencyKey: codecs.ulid.decode(ulid()),
      });
      assert.deepEqual(await reveal(instance), {
        value: "instance-key",
        source: "instance",
      });
      // A service added without its IDL shows what was set on its claim.
      assert.deepEqual(await reveal({ claimId: claim.claimId }), {
        value: "claim-key",
        source: "deployment",
      });
      // A plain setting is no secret to reveal.
      await assert.rejects(
        keeper
          .configSecretsReveal({ target: deployment, field: "endpoint" })
          .orThrow(),
        /no secret named endpoint/,
      );

      // A role that changes settings but not secrets is told what it needs.
      await assert.rejects(
        tuner
          .configSecretsReveal({ target: deployment, field: "apiKey" })
          .orThrow(),
        (error) => {
          assert(error instanceof apis.auth.AuthError);
          assert.equal(error.data.code, "access_denied");
          assert.match(
            error.message,
            /showing a secret needs trellis\.auth\.v1:config\.manageSecrets/,
          );
          return true;
        },
      );

      // Each reveal is on record with who asked, the target, and the field;
      // no audit record anywhere holds a secret's value.
      const platform = readdirSync(runtime.workdir, { recursive: true })
        .map(String)
        .find((path) => path.endsWith("platform.sqlite"));
      assert(platform, "the runtime keeps a platform database");
      const audit = () => {
        const database = new DatabaseSync(join(runtime.workdir, platform), {
          readOnly: true,
        });
        try {
          return database
            .prepare(
              "SELECT kind, actor_principal_id AS actor, target_id AS target, data_json AS data FROM auth_security_audit",
            )
            .all()
            .map((row) => ({
              kind: String(row.kind),
              actor: String(row.actor),
              target: String(row.target),
              data: String(row.data),
            }));
        } finally {
          database.close();
        }
      };
      const records = audit();
      const reveals = records.filter(
        (record) => record.kind === "rpc.config.secrets.reveal",
      );
      assert.deepEqual(
        reveals.map((record) => {
          const data = JSON.parse(record.data) as {
            field: string;
            source: string;
          };
          return [record.target.split(":")[0], data.field, data.source];
        }),
        [
          ["deployment", "apiKey", "deployment"],
          ["instance", "apiKey", "deployment"],
          ["instance", "apiKey", "instance"],
          ["claim", "apiKey", "deployment"],
        ],
      );
      assert.equal(new Set(reveals.map((record) => record.actor)).size, 1);
      for (const value of ["deployment-key", "instance-key", "claim-key"])
        assert(
          records.every(
            (record) =>
              !record.data.includes(value) && !record.target.includes(value),
          ),
          `an audit record holds the secret ${value}`,
        );

      // Reveals are limited per administrator: after 30 within ten minutes
      // the next is refused until the window passes.
      for (let shown = reveals.length; shown < 30; shown++)
        await reveal(deployment);
      await assert.rejects(reveal(deployment), (error) => {
        assert(error instanceof apis.auth.AuthError);
        assert.equal(error.data.code, "rate_limited");
        return true;
      });
      assert.equal(
        audit().filter((record) => record.kind === "rpc.config.secrets.reveal")
          .length,
        30,
      );
    } finally {
      await keeper.stop();
      await tuner.stop();
    }
  });
});
