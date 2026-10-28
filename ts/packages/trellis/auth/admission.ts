import type { NatsConnection } from "@nats-io/nats-core";
import { QueuedIteratorImpl } from "@nats-io/nats-core/internal";
import { ulid } from "ulid";
import type { SessionAuthorityOwner } from "./authority.ts";
import { type AdmissionContext, type AdmissionGet } from "./protocol_wasm.ts";
import type { TrellisAuth } from "./session_auth.ts";
import { base64urlDecode, base64urlEncode, sha256, utf8 } from "./utils.ts";

/** Obtain exact own-context bytes through the native signed-admission protocol. No application I/O is enabled here. */
export async function requestOwnAdmission(options: {
  nc: NatsConnection;
  owner: SessionAuthorityOwner;
  auth: TrellisAuth;
  runtimeId: string;
  attachmentId: string;
  timeoutMs: number;
  signal?: AbortSignal;
}): Promise<AdmissionContext> {
  const { nc, owner, auth, runtimeId, signal } = options;
  const deadline = Date.now() + options.timeoutMs;
  const keyDigest = base64urlEncode(
    await sha256(base64urlDecode(auth.sessionKey)),
  );
  const subject = `trellis.auth.admission.${keyDigest}.get`;
  const reply = `_INBOX.trellis.${keyDigest}.${ulid()}`;
  const request: AdmissionGet = {
    trellisInstanceId: owner.instanceId,
    runtimeId,
    sessionPublicKey: auth.sessionKey,
    attachmentId: options.attachmentId,
    requestNonce: base64urlEncode(crypto.getRandomValues(new Uint8Array(16))),
    issuedAt: auth.currentIat(),
  };
  const signedRequest = await auth.signAdmissionGet(request, subject, reply);
  if (signal?.aborted) throw signal.reason;
  if (Date.now() >= deadline) throw new Error("AdmissionTimeout");
  const pending = Promise.withResolvers<AdmissionContext>();
  let settled = false;
  let verifying = 0;
  const sub = nc.subscribe(reply, {
    callback: (error, message) => {
      if (settled) return;
      if (error) {
        pending.reject(error);
        return;
      }
      if (verifying >= 8 || message.data.length > 2 * 1024 * 1024) return;
      verifying++;
      void owner
        .verifyAdmissionContext(message.data, request, subject, reply)
        .then((response) => {
          if (settled || signal?.aborted || Date.now() >= deadline) return;
          settled = true;
          pending.resolve(response);
        })
        .catch(() => {
          /* Forged candidates never consume the legitimate pending lookup. */
        })
        .finally(() => {
          verifying--;
        });
    },
  });
  const statuses = nc.status();
  if (!(statuses instanceof QueuedIteratorImpl)) {
    sub.unsubscribe();
    throw new Error("AdmissionStatusUnavailable");
  }
  const statusTask = (async () => {
    for await (const status of statuses) {
      if (status.type === "disconnect" || status.type === "close") {
        pending.reject(new Error("AdmissionDisconnected"));
        break;
      }
    }
  })().catch((error) => pending.reject(error));
  const abort = () =>
    pending.reject(signal?.reason ?? new DOMException("Aborted", "AbortError"));
  const timer = setTimeout(
    () => pending.reject(new Error("AdmissionTimeout")),
    Math.max(0, deadline - Date.now()),
  );
  signal?.addEventListener("abort", abort, { once: true });
  try {
    if (signal?.aborted) throw signal.reason;
    nc.publish(subject, utf8(signedRequest), {
      reply,
    });
    return await pending.promise;
  } finally {
    settled = true;
    clearTimeout(timer);
    signal?.removeEventListener("abort", abort);
    sub.unsubscribe();
    statuses.stop();
    await statusTask;
  }
}
