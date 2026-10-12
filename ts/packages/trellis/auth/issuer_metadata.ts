import { createInbox, type NatsConnection } from "@nats-io/nats-core";
import {
  issuerMetadataSubjectWasm,
  verifyIssuerMetadataWasm,
  type AuthorityIssuerKey,
  type IssuerMetadataQuery,
  type IssuerMetadataRequest,
  type IssuerMetadataResponse,
} from "./protocol_wasm.ts";

/** Perform a bounded issuer-signed public-material read on an already admitted socket. */
export async function requestIssuerMetadata(options: {
  nats: NatsConnection;
  inboxPrefix: string;
  instanceId: string;
  account: string;
  query: IssuerMetadataQuery;
  /** Resolve only from authenticated pins or their verified rotation chain. */
  issuer: (keyId: string) => Promise<AuthorityIssuerKey>;
  assertCurrent: () => void;
  timeoutMs?: number;
  signal?: AbortSignal;
}): Promise<IssuerMetadataResponse> {
  options.assertCurrent();
  const subject = await issuerMetadataSubjectWasm();
  const reply = `${createInbox(options.inboxPrefix)}.metadata`;
  const nonce = crypto.getRandomValues(new Uint8Array(16));
  const request: IssuerMetadataRequest = {
    trellisInstanceId: options.instanceId,
    audienceNatsAccount: options.account,
    requestNonce: btoa(String.fromCharCode(...nonce))
      .replaceAll("+", "-")
      .replaceAll("/", "_")
      .replace(/=+$/u, ""),
    query: Object.freeze({ ...options.query }),
  };
  const pending = Promise.withResolvers<IssuerMetadataResponse>();
  let bytes = 0;
  let candidates = 0;
  let ended = false;
  const subscription = options.nats.subscribe(reply, {
    callback: (error, message) => {
      if (error) {
        pending.reject(error);
        return;
      }
      if (message.subject !== reply) return;
      bytes += message.data.length;
      if (bytes > 1024 * 1024 || ++candidates > 16) {
        pending.reject(new Error("issuer metadata response budget exceeded"));
        return;
      }
      // Serial verification prevents forged candidates from multiplying cold reads.
      ordered = ordered.then(async () => {
        if (ended) return;
        try {
          const hints = JSON.parse(new TextDecoder().decode(message.data)) as {
            response?: { issuerKeyId?: unknown };
          };
          if (typeof hints.response?.issuerKeyId !== "string") return;
          const issuer = await options.issuer(hints.response.issuerKeyId);
          options.assertCurrent();
          const verified = await verifyIssuerMetadataWasm(message.data, {
            issuer,
            request,
            requestSubject: subject,
            replySubject: message.subject,
            nowUnixSeconds: Math.floor(Date.now() / 1000),
          });
          options.assertCurrent();
          if (!ended) pending.resolve(verified);
        } catch {
          /* An untrusted response cannot consume the retained request. */
        }
      });
    },
  });
  let ordered = Promise.resolve();
  const abort = () =>
    pending.reject(new Error("issuer metadata request cancelled"));
  const timer = setTimeout(
    () => pending.reject(new Error("issuer metadata unavailable")),
    options.timeoutMs ?? 3000,
  );
  options.signal?.addEventListener("abort", abort, { once: true });
  try {
    if (options.signal?.aborted)
      throw new Error("issuer metadata request cancelled");
    options.assertCurrent();
    options.nats.publish(
      subject,
      new TextEncoder().encode(JSON.stringify(request)),
      { reply },
    );
    return await pending.promise;
  } finally {
    ended = true;
    clearTimeout(timer);
    subscription.unsubscribe();
    options.signal?.removeEventListener("abort", abort);
  }
}
