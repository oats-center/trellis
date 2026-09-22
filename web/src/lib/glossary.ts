/**
 * Self-contained operator glossary. Definitions are the in-console source of
 * truth for jargon popovers; keep each entry at most ~25 words and phrase it
 * for an operator mid-incident, not for a design doc.
 */
export const GLOSSARY: Record<string, string> = {
  "DLQ":
    "Where jobs land after exhausting every retry. Dead-lettered jobs stay inspectable until an operator replays or dismisses them.",
  "dead-lettered":
    "Terminal state after all retries are exhausted. The job remains stored for inspection, replay, or dismissal.",
  "reconciliation":
    "The pass that reads authority inputs, validates evidence, and replaces materialized permissions atomically. Stale evidence fails closed.",
  "contract digest":
    "The hash identifying the exact contract artifact a grant or permission was derived from.",
  "capability":
    "A named permission an app or service must hold before it can call a contract method.",
  "permission atom":
    "The indivisible permission unit that a route, operation, or control action resolves to.",
  "operation":
    "A caller-visible asynchronous workflow with status, signals, and results. Service-private execution uses jobs instead.",
  "job stream":
    "The durable JetStream subject where a service's jobs are stored, retried, and dead-lettered.",
  "inbox prefix":
    "The server-issued NATS reply subject prefix bound to a session's authorized transport.",
  "activation review":
    "The human decision step that approves or rejects a pending device activation before authority is granted.",
  "provisioning secret":
    "A one-time credential that lets a device or service complete enrollment. Trellis stores only its hash.",
  "heartbeat":
    "A periodic report a service instance emits so the runtime and console can judge its health.",
  "participant artifact":
    "The exact published contract document describing what a service or app is allowed to do.",
  "needs digest":
    "The hash of a participant's declared dependency requirements, used to key authorization evidence.",
  "grant set":
    "The effective bundle of permissions a session was issued, bound to a snapshot digest.",
  "session key":
    "The Ed25519 identity a client signs with to authenticate its NATS connection.",
  "context digest":
    "The hash naming a signed authorization context snapshot that admitted a connection.",
  "authorization context":
    "A signed, revocable record binding a session to its accepted grants and expiry.",
  "instance":
    "One enrolled device with its own lifecycle state, keyed by device principal and deployment.",
  "revocation":
    "Durable invalidation of a session, grant, or context. Revoked material can no longer admit connections.",
};
