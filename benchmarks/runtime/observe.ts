import type { NatsConnection } from "@nats-io/nats-core";

/** Passive same-broker diagnostics. Run separately from unobserved throughput:
 * the observer receives an extra copy of traffic and is not free instrumentation.
 * Only bounded aggregate timings/counts are retained, never bodies or identities.
 */
export async function observeTransfer(nc: NatsConnection) {
  const frameBytes = new Map<string, Map<number, number>>();
  const timings = new Map<
    string,
    { count: number; sumMs: number; maxMs: number; buckets: number[] }
  >();
  const bounds = [0.1, 0.25, 0.5, 1, 2, 5, 10, 25, 100, 1000];
  const pending = new Map<string, { kind: string; started: number }>();
  let infos = 0;
  let droppedCorrelations = 0;
  let frameSizeOverflows = 0;
  const subscription = nc.subscribe(">", {
    callback(error, msg) {
      if (error) throw error;
      const now = performance.now();
      const request = pending.get(msg.subject);
      if (request) {
        pending.delete(msg.subject);
        const ms = now - request.started;
        let metric = timings.get(request.kind);
        if (!metric) {
          metric = {
            count: 0,
            sumMs: 0,
            maxMs: 0,
            buckets: Array(bounds.length + 1).fill(0),
          };
          timings.set(request.kind, metric);
        }
        metric.count++;
        metric.sumMs += ms;
        metric.maxMs = Math.max(metric.maxMs, ms);
        const bucket = bounds.findIndex((bound) => ms <= bound);
        metric.buckets[bucket < 0 ? bounds.length : bucket]++;
      }
      const subject = msg.subject;
      if (
        subject.startsWith("transfer.v1.") &&
        msg.headers?.get("trellis-transfer-control") === "data"
      ) {
        const direction = subject.startsWith("transfer.v1.upload.")
          ? "upload"
          : "download";
        let sizes = frameBytes.get(direction);
        if (!sizes) {
          sizes = new Map();
          frameBytes.set(direction, sizes);
        }
        if (sizes.size < 1024 || sizes.has(msg.data.length)) {
          sizes.set(msg.data.length, (sizes.get(msg.data.length) ?? 0) + 1);
        } else frameSizeOverflows++;
      }
      if (subject.startsWith("$JS.API.CONSUMER.INFO.OBJ_")) infos++;
      const kind =
        subject.startsWith("$O.") && subject.includes(".C.")
          ? "storage-chunk-ack"
          : subject.startsWith("$O.") && subject.includes(".M.")
            ? "storage-metadata-ack"
            : subject.startsWith("$JS.API.CONSUMER.MSG.NEXT.OBJ_")
              ? "storage-fetch-first-response"
              : undefined;
      if (kind && msg.reply) {
        if (pending.size >= 4096) {
          pending.delete(pending.keys().next().value!);
          droppedCorrelations++;
        }
        pending.set(msg.reply, { kind, started: now });
      }
    },
  });
  await nc.flush();
  return async () => {
    await nc.flush();
    subscription.unsubscribe();
    await nc.close();
    return {
      interpretation:
        "Passive extra-copy diagnostic run; ACK timing starts at broker-observed publication, not SDK enqueue. Fetch first response may be a status. Not a measure of fsync or sender credit wait.",
      frameBytes: Object.fromEntries(
        Array.from(frameBytes, ([direction, sizes]) => [
          direction,
          Object.fromEntries(sizes),
        ]),
      ),
      consumerInfoRequests: infos,
      timingBoundsMs: [...bounds, "+Inf"],
      timings: Object.fromEntries(timings),
      droppedCorrelations,
      frameSizeOverflows,
      unansweredRequests: pending.size,
    };
  };
}
