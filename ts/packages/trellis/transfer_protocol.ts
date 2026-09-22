const TRANSFER_FRAME_PROOF_DOMAIN = new TextEncoder().encode(
  "trellis.transfer.v1.frame\0",
);

export function transferFrameProofPayload(
  seq: number,
  control: string | undefined,
  payload: Uint8Array,
): Uint8Array {
  const controlBytes = new TextEncoder().encode(control ?? "");
  const framed = new Uint8Array(
    TRANSFER_FRAME_PROOF_DOMAIN.length + 8 + 4 + controlBytes.length +
      payload.length,
  );
  framed.set(TRANSFER_FRAME_PROOF_DOMAIN);
  const view = new DataView(framed.buffer);
  view.setBigUint64(TRANSFER_FRAME_PROOF_DOMAIN.length, BigInt(seq));
  view.setUint32(TRANSFER_FRAME_PROOF_DOMAIN.length + 8, controlBytes.length);
  framed.set(controlBytes, TRANSFER_FRAME_PROOF_DOMAIN.length + 12);
  framed.set(
    payload,
    TRANSFER_FRAME_PROOF_DOMAIN.length + 12 + controlBytes.length,
  );
  return framed;
}
