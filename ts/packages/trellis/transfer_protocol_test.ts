import { assertEquals } from "@std/assert";
import vectors from "../../../conformance/transfer-v1-vectors.json" with {
  type: "json",
};
import { base64urlDecode, base64urlEncode } from "./auth/utils.ts";
import { transferFrameProofPayload } from "./transfer_protocol.ts";

Deno.test("transfer v1 authenticated framing matches shared vectors", () => {
  for (const vector of vectors.vectors) {
    assertEquals(
      base64urlEncode(
        transferFrameProofPayload(
          vector.seq,
          vector.control,
          base64urlDecode(vector.payloadBase64url),
        ),
      ),
      vector.framedBase64url,
    );
  }
});
