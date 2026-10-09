import init, {
  create_authorization_context_handle,
  live_server_proof_digest,
  live_verify_server_proof,
  transfer_frame_digest,
  transfer_server_proof_digest,
  transfer_verify_server_proof,
  verify_authorization_event,
  verify_authorization_request,
  verify_transfer_authorization_request,
} from "../protocol_wasm/trellis_protocol_wasm.js";

void (async () => {
  const node = !globalThis.Deno && !!globalThis.process?.versions?.node;
  const port = node
    ? (await import("node:worker_threads")).parentPort
    : globalThis;
  const url = new URL(
    "../protocol_wasm/trellis_protocol_wasm_bg.wasm",
    import.meta.url,
  );
  const bytes = url.protocol === "file:"
    ? globalThis.Deno
      ? await Deno.readFile(url)
      : await (await import("node:fs/promises")).readFile(url)
    : await (await fetch(url)).arrayBuffer();
  await init({ module_or_path: bytes });
  const contexts = new Map();
  const receive = (message) => {
    const { id, contextId } = message;
    const started = performance.now();
    let verificationStarted;
    let verifyMs = 0;
    try {
      if (message.kind === "release") {
        contexts.get(contextId)?.free();
        contexts.delete(contextId);
        port.postMessage({ id });
        return;
      }
      const body = new Uint8Array(message.payload);
      verificationStarted = performance.now();
      let frameResult;
      switch (message.kind) {
        case "live-digest":
          frameResult = {
            kind: "digest",
            value: live_server_proof_digest(
              message.contextDigest,
              message.subject,
              body,
            ),
          };
          break;
        case "transfer-digest":
          frameResult = {
            kind: "digest",
            value: transfer_server_proof_digest(
              message.contextDigest,
              message.subject,
              JSON.stringify(message.descriptor),
              body,
            ),
          };
          break;
        case "transfer-frame-digest":
          frameResult = {
            kind: "digest",
            value: transfer_frame_digest(
              JSON.stringify(message.descriptor),
              body,
            ),
          };
          break;
        case "live-verify":
          live_verify_server_proof(
            message.proof,
            message.contextDigest,
            message.subject,
            body,
            message.providerKey,
          );
          frameResult = { kind: "verified" };
          break;
        case "transfer-verify":
          transfer_verify_server_proof(
            message.proof,
            message.contextDigest,
            message.subject,
            JSON.stringify(message.descriptor),
            body,
            message.providerKey,
          );
          frameResult = { kind: "verified" };
          break;
      }
      if (frameResult) {
        port.postMessage({
          id,
          result: frameResult,
          timing: {
            totalMs: performance.now() - started,
            verifyMs: performance.now() - verificationStarted,
          },
        });
        return;
      }
      const policy = {
        ...message.policy,
        nowUnixSeconds: message.policy.nowUnixSeconds +
          Math.floor(Math.max(0, Date.now() - message.sentAt) / 1000),
      };
      let context = contexts.get(contextId);
      if (!context) {
        if (!message.context) {
          throw new Error("Verification context was not installed");
        }
        context = create_authorization_context_handle(
          JSON.stringify(message.context.issuer),
          JSON.stringify(message.context.signed),
          JSON.stringify(policy),
          message.kind === "event",
        );
        if (
          JSON.parse(context.projection()).contextDigest !==
            message.context.digest
        ) {
          context.free();
          throw new Error("Verification context digest mismatch");
        }
        contexts.set(contextId, context);
      }
      const request = JSON.stringify({ ...message.input, policy });
      verificationStarted = performance.now();
      const result = message.kind === "event"
        ? verify_authorization_event(context, request, body)
        : message.transfer
        ? verify_transfer_authorization_request(
          context,
          request,
          body,
          message.transfer.providerConnectionId,
          message.transfer.consumerConnectionId,
          message.transfer.transferId,
        )
        : verify_authorization_request(context, request, body);
      verifyMs = performance.now() - verificationStarted;
      const parsed = JSON.parse(result);
      port.postMessage({
        id,
        result: { kind: message.kind, value: parsed },
        timing: { totalMs: performance.now() - started, verifyMs },
      });
    } catch (error) {
      if (verificationStarted !== undefined) {
        verifyMs = performance.now() - verificationStarted;
      }
      port.postMessage({
        id,
        error: String(error),
        timing: { totalMs: performance.now() - started, verifyMs },
      });
    }
  };
  if (node) port.on("message", receive);
  else globalThis.onmessage = (event) => receive(event.data);
  port.postMessage({ ready: true });
})().catch((error) => {
  // Surface failed initialization through the ordinary worker error channel.
  setTimeout(() => {
    throw error;
  }, 0);
});
