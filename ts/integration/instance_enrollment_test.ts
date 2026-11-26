/**
 * Service processes that hold an enrollment secret register themselves as
 * new instances of their deployment, keep or retire that instance, and stop
 * when an administrator removed it.
 */
import assert from "node:assert/strict";
import { mkdtemp, readFile } from "node:fs/promises";
import { join } from "node:path";
import { test } from "node:test";
import { ulid } from "ulid";
import { Result } from "@oatscenter/trellis";
import { codecs } from "@oatscenter/trellis/generated";
import { TrellisService } from "@oatscenter/trellis/service";
import type { TrellisTestRuntime } from "@oatscenter/trellis-testkit";
import {
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

const participant = participants.Provider.participant;

async function deployment(runtime: TrellisTestRuntime, name: string) {
  const { deploymentId } = await runtime.contracts.apply({
    deployment: name,
    contract: participant,
  });
  assert(deploymentId, "applying the participant created a deployment");
  return codecs.ulid.decode(deploymentId);
}

async function issue(
  runtime: TrellisTestRuntime,
  deploymentId: ReturnType<typeof codecs.ulid.decode>,
  limits: { maxInstances?: number } = {},
) {
  const issued = await runtime.callAdminRpc("authEnrollmentSecretsIssue", {
    deploymentId,
    title: "test pods",
    ...limits,
    idempotencyKey: codecs.ulid.decode(ulid()),
  });
  assert(issued.text);
  return { secret: issued.secret, text: issued.text };
}

async function instances(
  runtime: TrellisTestRuntime,
  deploymentId: ReturnType<typeof codecs.ulid.decode>,
) {
  return (
    await runtime.callAdminRpc("authServiceInstancesList", { deploymentId })
  ).items;
}

/** The messages of an error and everything that caused it. */
function described(error: unknown): string {
  const messages: string[] = [];
  for (let at = error; at instanceof Error; at = at.cause)
    messages.push(at.message);
  return messages.join(": ");
}

test("a process with an enrollment secret enrolls, serves, and retires itself when it stops", async () => {
  await withTrellisRuntime(async (runtime) => {
    const deploymentId = await deployment(runtime, "enrolled-provider");
    const { secret, text } = await issue(runtime, deploymentId, {
      maxInstances: 1,
    });
    const service = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { secret: text, title: "pod-a" },
    }).orThrow();
    await service.handleEcho(({ input }) => Result.ok(input));
    const caller = await runtime.connectClient({
      name: "enrolled-caller",
      contract: apps.Caller.request,
    });
    assert.deepEqual(await caller.echo({ value: "hello" }).orThrow(), {
      value: "hello",
    });
    const [enrolled] = await instances(runtime, deploymentId);
    assert(enrolled);
    assert.equal(enrolled.origin.kind, "enrolled");
    assert.equal(enrolled.origin.secretId, secret.secretId);
    assert.equal(enrolled.principal.title, "pod-a");
    assert(enrolled.connectedAt);

    // At its cap of one, with that one connected, there is nothing to replace.
    const second = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { secret: text },
    });
    assert(second.isErr());
    assert.match(described(second.error), /instance_limit/);

    await service.stop();
    const [stopped] = await instances(runtime, deploymentId);
    assert.equal(stopped?.state, "retired");
    assert.equal(stopped?.retiredReason, "stopped");
  });
});

test("a key-file host restarts with its stored key after the secret was rotated", async () => {
  await withTrellisRuntime(async (runtime) => {
    const deploymentId = await deployment(runtime, "key-file-provider");
    const { secret, text } = await issue(runtime, deploymentId);
    const keyFile = join(
      await mkdtemp(join(runtime.workdir, "enroll-")),
      "key",
    );
    const first = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { secret: text, keyFile },
    }).orThrow();
    const stored = await readFile(keyFile, "utf8");
    await first.stop();
    const [kept] = await instances(runtime, deploymentId);
    // A stored key outlives the process, so a stop does not retire it.
    assert.equal(kept?.state, "active");

    await runtime.callAdminRpc("authEnrollmentSecretsRevoke", {
      secretId: secret.secretId,
      expectedRevision: secret.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    // The restart needs no secret at all: the stored key connects.
    const restarted = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { keyFile },
    }).orThrow();
    try {
      const listed = await instances(runtime, deploymentId);
      assert.equal(listed.length, 1);
      assert.equal(listed[0]?.instanceId, kept?.instanceId);
      assert(listed[0]?.connectedAt);
      assert.equal(await readFile(keyFile, "utf8"), stored);
    } finally {
      await restarted.stop();
    }
  });
});

test("a key-file host whose instance an administrator removed stops instead of enrolling again", async () => {
  await withTrellisRuntime(async (runtime) => {
    const deploymentId = await deployment(runtime, "removed-provider");
    const { text } = await issue(runtime, deploymentId);
    const keyFile = join(
      await mkdtemp(join(runtime.workdir, "enroll-")),
      "key",
    );
    const first = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { secret: text, keyFile },
    }).orThrow();
    await first.stop();
    const [instance] = await instances(runtime, deploymentId);
    assert(instance);
    await runtime.callAdminRpc("authServiceInstancesRemove", {
      instanceId: instance.instanceId,
      expectedRevision: instance.revision,
      idempotencyKey: codecs.ulid.decode(ulid()),
    });
    // It still holds a valid secret, yet the removal stands.
    const restarted = await TrellisService.connect({
      trellisUrl: runtime.trellisUrl,
      participant,
      enrollment: { secret: text, keyFile },
    });
    assert(restarted.isErr());
    assert.match(described(restarted.error), /administrator removed/);
    const listed = await instances(runtime, deploymentId);
    assert.deepEqual(
      listed.map((item) => item.state),
      ["removed"],
    );
  });
});
