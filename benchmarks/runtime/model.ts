import { z } from "zod";
import { createHash } from "node:crypto";

/** Validated options shared by the provider, storage probe, and load generator. */
export const WorkerOptions = z.object({
  role: z.enum(["provider", "storage", "client"]),
  output: z.string(),
  trellisUrl: z.url(),
  httpUrl: z.url().optional(),
  serviceSeed: z.string().optional(),
  credentialHandles: z.array(z.ulid()).default([]),
  username: z.string(),
  password: z.string(),
  samples: z.number().int().positive(),
  calls: z.number().int().positive(),
  sizes: z.array(
    z
      .number()
      .int()
      .positive()
      .max(512 * 1024 * 1024),
  ),
  sessionCounts: z.array(z.number().int().positive()),
  idleSeconds: z.number().positive(),
  cpuTicks: z.number().int().positive(),
  arrivalRate: z.number().nonnegative(),
  maxOutstanding: z.number().int().positive(),
  providerIndex: z.number().int().nonnegative().default(0),
  workload: z.enum(["all", "transfer", "lifecycle"]).default("all"),
  warmups: z.number().int().nonnegative().default(0),
  /** One declared HTTP body chunk policy shared by both languages' clients and servers. */
  httpChunkBytes: z
    .number()
    .int()
    .positive()
    .default(64 * 1024),
  /** Directory of generated CA, mutual-TLS and request-signing material; absent means plaintext. */
  httpTlsDir: z.string().optional(),
});

/** HTTP transfer server behaviours, matched to the Trellis workload categories. */
export const HTTP_MODES = ["raw", "stream", "store"] as const;
/** One HTTP transfer server behaviour. */
export type HttpMode = (typeof HTTP_MODES)[number];

/** One observable operation, including failures rather than success-only timing. */
export type Sample = {
  scenario: string;
  transport: "trellis" | "http" | "store" | "nats";
  warmup?: boolean;
  dataFrames?: number;
  maxFrameBytes?: number;
  windowFrames?: number;
  windowBytes?: number;
  startedUnixMs: number;
  durationMs: number;
  bytes?: number;
  setupMs?: number;
  firstByteMs?: number;
  payloadSuppliedMs?: number;
  committedMs?: number;
  connectMs?: number;
  firstRpcMs?: number;
  cancellationMs?: number;
  operations?: number;
  sessions?: number;
  error?: string;
  offeredUnixMs?: number;
  schedulerDelayMs?: number;
  loadGeneratorDrop?: boolean;
  documentTtfbMs?: number;
  navigationReadyMs?: number;
  transferredBytes?: number;
  /** Wall-clock milliseconds from start until the receiver verified size and digest. */
  verifiedMs?: number;
  /** Wall-clock milliseconds from start until the required durable write completed. */
  durableMs?: number;
};

/** Deterministic binary fixture; generation is outside timed transfer work. */
export function payload(size: number): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(size);
  for (let index = 0; index < size; index++) bytes[index] = index % 251;
  return bytes;
}

/** Repeat the binary fixture with bounded immutable chunks, independent of size. */
export async function* payloadChunks(
  size: number,
  chunkBytes = 64 * 1024,
): AsyncGenerator<Uint8Array> {
  if (
    !Number.isSafeInteger(size) ||
    size < 0 ||
    !Number.isSafeInteger(chunkBytes) ||
    chunkBytes < 1
  ) {
    throw new Error("Invalid streaming fixture size");
  }
  const pattern = payload(chunkBytes + 250);
  for (let offset = 0; offset < size; offset += chunkBytes) {
    const start = offset % 251;
    yield pattern.subarray(start, start + Math.min(chunkBytes, size - offset));
  }
}

/** Hash a streamed body without retaining delivered chunks. */
export async function streamDigest(
  source: AsyncIterable<Uint8Array>,
): Promise<{ size: number; digest: string }> {
  const hash = createHash("sha256");
  let size = 0;
  for await (const chunk of source) {
    size += chunk.length;
    hash.update(chunk);
  }
  return { size, digest: hash.digest("hex") };
}

/** SHA-256 of a completed body, for independent transfer integrity verification. */
export async function digest(bytes: Uint8Array): Promise<string> {
  const hash = await crypto.subtle.digest("SHA-256", new Uint8Array(bytes));
  return Array.from(new Uint8Array(hash), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

/** Bound event/stream completion without retrying or discarding failed samples. */
export async function deadline<T>(
  work: Promise<T>,
  milliseconds = 30_000,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      work,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`Workload exceeded ${milliseconds} ms`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** Summary plus raw samples; no histogram-bucket interpolation for percentiles. */
export function summarize(samples: Sample[]) {
  const groups = Map.groupBy(
    samples.filter((sample) => !sample.warmup),
    (sample) =>
      `${sample.transport}/${sample.scenario}/${sample.bytes ?? 0}/${
        sample.sessions ?? 1
      }`,
  );
  return Array.from(groups, ([name, rows]) => {
    const values = rows
      .filter((row) => !row.error)
      .map((row) => row.durationMs)
      .sort((a, b) => a - b);
    const percentile = (fraction: number) =>
      values.length
        ? values[Math.max(0, Math.ceil(values.length * fraction) - 1)]
        : null;
    return {
      name,
      attempts: rows.length,
      errors: rows.filter((row) => row.error).length,
      submitted: rows.filter((row) => !row.loadGeneratorDrop).length,
      drops: rows.filter((row) => row.loadGeneratorDrop).length,
      phases: Object.fromEntries(
        (
          [
            "connectMs",
            "firstRpcMs",
            "setupMs",
            "firstByteMs",
            "payloadSuppliedMs",
            "committedMs",
            "cancellationMs",
            "schedulerDelayMs",
            "documentTtfbMs",
            "navigationReadyMs",
            "transferredBytes",
            "verifiedMs",
            "durableMs",
          ] as const
        ).map((key) => {
          const values = rows
            .filter((row) => !row.error && row[key] !== undefined)
            .map((row) => row[key]!)
            .sort((a, b) => a - b);
          return [
            key,
            values.length
              ? (values[Math.floor((values.length - 1) / 2)] +
                  values[Math.floor(values.length / 2)]) /
                2
              : null,
          ];
        }),
      ),
      medianMs: values.length
        ? (values[Math.floor((values.length - 1) / 2)] +
            values[Math.floor(values.length / 2)]) /
          2
        : null,
      p95Ms: values.length >= 20 ? percentile(0.95) : null,
      // Small connect/transfer cohorts do not support a useful p99 claim.
      p99Ms: values.length >= 1000 ? percentile(0.99) : null,
    };
  });
}
