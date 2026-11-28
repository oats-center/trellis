import assert from "node:assert/strict";
import { test } from "node:test";

import { isErr, isOk } from "@oatscenter/result";

import { newId, parseId } from "./values.ts";

test("an ID parses bare or with its kind's prefix, kept as given", () => {
  const bare = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
  for (const text of [bare, `S${bare}`, `D${bare}`]) {
    const parsed = parseId(text, "serviceDeployment", "deviceDeployment");
    assert.ok(isOk(parsed), text);
    assert.equal(parsed.take() as string, text);
  }
  // Another kind's prefix, lowercase, or anything that would change a NATS
  // subject is refused.
  for (const text of [
    `U${bare}`,
    `SI${bare}`,
    `s${bare}`,
    bare.toLowerCase(),
    `S${bare}.>`,
    `S*${bare}`,
    `SXY${bare}`,
    "",
  ]) {
    assert.ok(
      isErr(parseId(text, "serviceDeployment", "deviceDeployment")),
      text,
    );
  }
  const job = newId("job") as string;
  assert.match(job, /^J[0-7][0-9A-HJKMNP-TV-Z]{25}$/);
  assert.ok(isOk(parseId(job, "job")));
  assert.ok(isErr(parseId(job, "operation")));
});
