import type {
  JobsHistoryRequest,
  JobsHistoryResponse,
} from "../../../internal_sdk/generated/types/index.js";

/** @internal Read resource-owned history at one fixed broker watermark. */
export async function readJobHistory(
  call: (input: JobsHistoryRequest) => Promise<JobsHistoryResponse>,
  queue: string,
  jobId: string,
): Promise<Array<{ seq: number; data: Uint8Array }>> {
  let fromSequence = 1n;
  let throughSequence: bigint | undefined;
  const entries: Array<{ seq: number; data: Uint8Array }> = [];
  for (;;) {
    const page = await call({ queue, jobId, fromSequence, throughSequence });
    if (
      throughSequence !== undefined &&
      throughSequence !== page.throughSequence
    ) {
      throw new Error("Jobs history watermark changed");
    }
    throughSequence = page.throughSequence;
    for (const entry of page.entries) {
      if (
        entry.sequence < fromSequence ||
        entry.sequence > page.throughSequence ||
        entry.sequence > BigInt(Number.MAX_SAFE_INTEGER)
      ) {
        throw new Error("Jobs history sequence out of range");
      }
      entries.push({ seq: Number(entry.sequence), data: entry.payload });
      fromSequence = entry.sequence + 1n;
    }
    if (page.nextSequence === undefined) return entries;
    if (
      page.nextSequence < fromSequence ||
      page.nextSequence > page.throughSequence
    ) {
      throw new Error("Jobs history cursor out of range");
    }
    fromSequence = page.nextSequence;
  }
}
