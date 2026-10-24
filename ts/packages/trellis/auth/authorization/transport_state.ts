import {
  classifyTransportAuthorizationWasm,
  TRANSPORT_AUTHORIZATION_FORMAT_V1,
  type TransportAuthorizationV1,
} from "../protocol_wasm.ts";

/**
 * Whether the admitted policy covers the exact subjects an operation needs.
 *
 * The need is checked as subject-union containment against A using the same
 * shared full-witness implementation as the runtime, so `a.>` covers `a.b` and
 * `a.*` does not cover `a.b.c`. Literal string-set subtraction is never used.
 * The response allowance and hard deadline are copied from A so this answers a
 * pure subject-coverage question.
 */
export async function admittedPolicyCovers(
  admitted: TransportAuthorizationV1,
  required: { publish?: readonly string[]; subscribe?: readonly string[] },
  nowUnixSeconds: number,
): Promise<boolean> {
  const need: TransportAuthorizationV1 = {
    format: TRANSPORT_AUTHORIZATION_FORMAT_V1,
    account: admitted.account,
    publishAllow: canonicalPatterns(required.publish),
    subscribeAllow: canonicalPatterns(required.subscribe),
    response: admitted.response,
    hardExpiresAt: admitted.hardExpiresAt,
  };
  const relation = await classifyTransportAuthorizationWasm(
    need,
    admitted,
    nowUnixSeconds,
  );
  return relation !== "reduction_required";
}

/** Canonicalize a subject-pattern list to the sorted, unique policy form. */
function canonicalPatterns(patterns: readonly string[] | undefined): string[] {
  return [...new Set(patterns ?? [])].sort();
}
