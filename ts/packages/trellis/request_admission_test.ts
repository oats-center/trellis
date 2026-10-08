import { assert, assertEquals } from "@std/assert";
import { RequestAdmission } from "./request_admission.ts";

Deno.test("reserved controls progress while ordinary and refusal capacity are exhausted", () => {
  const admission = new RequestAdmission({
    requests: 1,
    bytes: 100,
    controls: 1,
    controlBytes: 100,
  });
  const request = admission.admit(10, false);
  assert(request && !request.refused);
  const refusals = Array.from({ length: 4 }, () => {
    const permit = admission.admit(10, false);
    assert(permit?.refused);
    return permit;
  });
  assertEquals(admission.admit(10, false), undefined);
  const control = admission.admit(10, true);
  assert(control && !control.refused);
  for (const permit of [request, control, ...refusals]) permit.release();
  const next = admission.admit(10, false);
  assert(next && !next.refused);
  next.release();
});

Deno.test("byte refusal does not retain a count slot and releasing restores both capacities", () => {
  const admission = new RequestAdmission({ requests: 2, bytes: 4 });
  const first = admission.admit(3, false);
  assert(first && !first.refused);
  const refused = admission.admit(2, false);
  assert(refused?.refused);
  const lastByte = admission.admit(1, false);
  assert(lastByte && !lastByte.refused);
  for (const permit of [first, refused, lastByte]) {
    permit.release();
    permit.release();
  }
  const wholeBudget = admission.admit(4, false);
  assert(wholeBudget && !wholeBudget.refused);
  wholeBudget.release();
});
