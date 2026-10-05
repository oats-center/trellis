import { headers, type NatsConnection } from "@nats-io/nats-core";
import { deadline, digest, payload, type Sample } from "./model.ts";

/** Unsigned same-broker Core NATS lower bound; not an application transfer API. */
export async function rawTransfer(options: {
  connect: () => Promise<NatsConnection>;
  sizes: number[];
  samples: number;
  warmups: number;
}): Promise<Sample[]> {
  const sender = await options.connect();
  const rows: Sample[] = [];
  try {
    const receiver = await options.connect();
    try {
      for (const size of options.sizes) {
        const body = payload(size);
        const expected = await digest(body);
        // Diagnostic-only frame sizes: no public Trellis frame tuning knobs.
        for (const requested of [64 * 1024, 1024 * 1024]) {
          const frameBytes = Math.min(
            requested,
            sender.info!.max_payload - 4096,
          );
          if (frameBytes <= 0) {
            throw new Error("Broker payload limit is unusable");
          }
          for (let trial = -options.warmups; trial < options.samples; trial++) {
            const subject = `benchmark.raw.${crypto.randomUUID()}`;
            const data = receiver.subscribe(`${subject}.data`);
            const credit = sender.subscribe(`${subject}.credit`);
            const credits = credit[Symbol.asyncIterator]();
            await Promise.all([sender.flush(), receiver.flush()]);
            const row: Sample = {
              scenario: `windowed-stream-${frameBytes}`,
              transport: "nats",
              bytes: size,
              warmup: trial < 0,
              maxFrameBytes: frameBytes,
              startedUnixMs: Date.now(),
              durationMs: 0,
            };
            const started = performance.now();
            const frameCount = Math.ceil(size / frameBytes);
            const received = new Uint8Array(size);
            const read = (async () => {
              let sequence = 0;
              let offset = 0;
              let creditedSequence = 0;
              let creditedBytes = 0;
              for await (const message of data) {
                sequence++;
                if (
                  message.headers?.get("sequence") !== String(sequence) ||
                  message.data.length !== Math.min(frameBytes, size - offset)
                ) throw new Error("Raw stream sequence/length mismatch");
                received.set(message.data, offset);
                offset += message.data.length;
                if (
                  sequence - creditedSequence >= 8 ||
                  offset - creditedBytes >= 1024 * 1024 || offset === size
                ) {
                  receiver.publish(
                    `${subject}.credit`,
                    new TextEncoder().encode(String(sequence)),
                  );
                  creditedSequence = sequence;
                  creditedBytes = offset;
                }
                if (offset === size) break;
              }
              if (offset !== size || await digest(received) !== expected) {
                throw new Error("Corrupt raw Core NATS stream");
              }
              row.dataFrames = sequence;
            })();
            const write = (async () => {
              let sent = 0;
              let consumed = 0;
              const windowFrames = Math.min(
                16,
                Math.floor(4 * 1024 * 1024 / frameBytes),
              );
              while (sent < frameCount || consumed < sent) {
                if (sent < frameCount && sent - consumed < windowFrames) {
                  const header = headers();
                  header.set("sequence", String(++sent));
                  sender.publish(
                    `${subject}.data`,
                    body.subarray((sent - 1) * frameBytes, sent * frameBytes),
                    { headers: header },
                  );
                } else {
                  const next = await credits.next();
                  if (next.done) throw new Error("Raw credit stream closed");
                  const value = new TextDecoder().decode(next.value.data);
                  const cursor = Number(value);
                  if (
                    !Number.isSafeInteger(cursor) || String(cursor) !== value ||
                    cursor <= consumed || cursor > sent
                  ) {
                    throw new Error("Invalid raw cumulative credit");
                  }
                  consumed = cursor;
                }
              }
            })();
            try {
              await deadline(Promise.all([read, write]));
            } catch (error) {
              row.error = Deno.inspect(error, { colors: false, depth: 6 });
            } finally {
              row.durationMs = performance.now() - started;
              rows.push(row);
              data.unsubscribe();
              credit.unsubscribe();
              await Promise.allSettled([read, write]);
            }
          }
        }
      }
    } finally {
      await receiver.close();
    }
  } finally {
    await sender.close();
  }
  return rows;
}
