import assert from "node:assert/strict";
import { createInterface } from "node:readline";
import { ok } from "@oatscenter/result";
import { participants } from "../../../../integration/fixtures/testkit/packages/trellis-test-fixture/index.js";
import { TrellisService } from "../service/mod.ts";

const lines = createInterface({ input: process.stdin });
const input = await lines[Symbol.asyncIterator]().next();
if (input.done) throw new Error("provisioned identity required");
const { trellisUrl, seed } = JSON.parse(input.value) as {
  trellisUrl: string;
  seed: string;
};
const options = {
  trellisUrl,
  seed,
  participant: participants.JobsProvider.participant,
};
const submitter = await TrellisService.connect(options).orThrow();
try {
  const ref = await submitter.jobs.work
    .create({ value: "Node retained input" })
    .orThrow();
  const worker = await TrellisService.connect(options).orThrow();
  try {
    worker.jobs.work.handle(async ({ job }) => {
      return ok({ value: `${job.payload.value}: completed` });
    });
    const running = worker.wait();
    await Promise.race([ref.wait().orThrow(), running]);
  } finally {
    await worker.stop();
  }
  const snapshot = await ref.get().orThrow();
  assert.equal(snapshot.payload.value, "Node retained input");
  assert.equal(snapshot.result?.value, "Node retained input: completed");
  console.log(JSON.stringify({ stage: "verified" }));
} finally {
  await submitter.stop();
  lines.close();
}
