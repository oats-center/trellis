import { assert, assertEquals, assertThrows } from "@std/assert";
import {
  create_session_authority_handle,
  initSync,
  verify_session_event,
  verify_session_request,
} from "../../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm.js";
import { PROTOCOL_WASM_BASE64 } from "../../../ts/packages/trellis/auth/protocol_wasm/trellis_protocol_wasm_bytes.ts";

initSync({
  module: Uint8Array.from(
    atob(PROTOCOL_WASM_BASE64),
    (character) => character.charCodeAt(0),
  ),
});

const fixture = JSON.parse(await Deno.readTextFile(Deno.args[0]));
const handle = create_session_authority_handle(
  JSON.stringify(fixture.verification),
  new TextEncoder().encode(JSON.stringify(fixture.authority)),
);
try {
  const verify = (request: unknown, payload: string) =>
    JSON.parse(verify_session_request(
      handle,
      JSON.stringify({
        request,
        proof: fixture.proof,
        policy: fixture.verification.policy,
        knownRevoked: false,
      }),
      new TextEncoder().encode(payload),
    ));
  const result = verify(fixture.request, fixture.payload);
  assertEquals(result.ok, true);
  assertEquals(result.requestProofDigest, fixture.requestProofDigest);
  assertEquals(result.caller, fixture.caller);
  assert(
    !verify(
      { ...fixture.request, subject: `${fixture.request.subject}.other` },
      fixture.payload,
    ).ok,
  );
  assert(
    !verify({ ...fixture.request, acceptedRevision: "4" }, fixture.payload).ok,
  );
  assert(!verify(fixture.request, fixture.payload.replace("true", "false")).ok);
  const changed = structuredClone(fixture.authority);
  changed.binding.origin = "https://other.example";
  assertThrows(() =>
    create_session_authority_handle(
      JSON.stringify(fixture.verification),
      new TextEncoder().encode(JSON.stringify(changed)),
    )
  );
} finally {
  handle.free();
}

const historical = create_session_authority_handle(
  JSON.stringify(fixture.eventVerification),
  new TextEncoder().encode(JSON.stringify(fixture.authority)),
);
try {
  const verify = (
    event: unknown,
    originalPublication: unknown,
    payload = fixture.payload,
    proof = fixture.eventProof,
  ) =>
    JSON.parse(verify_session_event(
      historical,
      JSON.stringify({
        event,
        proof,
        policy: fixture.eventVerification.policy,
        originalPublication,
        knownRevoked: true,
      }),
      new TextEncoder().encode(payload),
    ));
  const retained = verify(fixture.event, fixture.originalPublication);
  assertEquals(retained.ok, true);
  assertEquals(retained.eventProofDigest, fixture.eventProofDigest);
  assertEquals(retained.publisherAuthority, fixture.publisherAuthority);
  assertEquals(retained.originalPublication, fixture.originalPublication);
  for (const candidate of fixture.signedTimeCases) {
    assertEquals(
      verify(
        candidate.event,
        fixture.originalPublication,
        fixture.payload,
        candidate.proof,
      ).ok,
      candidate.accepted,
      `signed event time cutoff: ${candidate.event.eventTime}`,
    );
  }
  assert(!verify(fixture.event, null).ok);
  assert(
    !verify(fixture.event, {
      ...fixture.originalPublication,
      publishedAt: "1970-01-01T00:20:00Z",
      streamSequence: "18",
    }).ok,
  );
  assert(
    !verify(
      { ...fixture.event, action: "Documents" },
      fixture.originalPublication,
    ).ok,
  );
  assert(
    !verify(
      fixture.event,
      fixture.originalPublication,
      fixture.payload.replace("true", "false"),
    ).ok,
  );
} finally {
  historical.free();
}
