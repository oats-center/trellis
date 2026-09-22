/** Per-target bulk-action execution shared by table pages. Selection state
 * itself stays on each page as a mutable `Set<string>` backed by `SvelteSet`. */

import { mapWithConcurrency } from "./console/paging.ts";

/** Maximum simultaneous bulk mutations; larger batches fan out in waves. */
export const MAX_BULK_CONCURRENCY = 4;

export function toggleId(selected: Set<string>, id: string): void {
  if (selected.has(id)) selected.delete(id);
  else selected.add(id);
}

export function toggleAll(selected: Set<string>, ids: string[]): void {
  const allSelected = ids.length > 0 && ids.every((id) => selected.has(id));
  if (allSelected) {
    for (const id of ids) selected.delete(id);
  } else {
    for (const id of ids) selected.add(id);
  }
}

/** Drops selected IDs that are absent or no longer eligible on this page. */
export function pruneSelection(
  selected: Set<string>,
  eligibleIds: readonly string[],
): void {
  const eligible = new Set(eligibleIds);
  for (const id of [...selected]) {
    if (!eligible.has(id)) selected.delete(id);
  }
}

/** One target's dispatch result, including the uncertain case. */
export type BulkTargetOutcome<T> =
  | { kind: "succeeded"; target: T }
  | { kind: "failed"; target: T; error: unknown }
  | { kind: "unknown"; target: T; error: unknown };

export type BulkOutcome<T> = {
  succeeded: number;
  failed: { target: T; reason: string }[];
  /** Outcomes whose send may or may not have applied; never auto-retried. */
  unknown: { target: T; reason: string }[];
  outcomes: BulkTargetOutcome<T>[];
};

/**
 * Runs `action` for every target with bounded concurrency, without stopping on
 * failure. `classify` decides whether a rejection is a definite failure or an
 * uncertain outcome; both are reported per target so partial results stay
 * visible and nothing is silently retried.
 */
export async function runBulk<T>(
  targets: readonly T[],
  action: (target: T) => Promise<void>,
  classify?: (error: unknown) => "failed" | "unknown",
): Promise<BulkOutcome<T>> {
  const outcomes = await mapWithConcurrency(
    targets,
    MAX_BULK_CONCURRENCY,
    async (target): Promise<BulkTargetOutcome<T>> => {
      try {
        await action(target);
        return { kind: "succeeded", target };
      } catch (error) {
        return {
          kind: classify?.(error) ?? "failed",
          target,
          error,
        } as BulkTargetOutcome<T>;
      }
    },
  );
  const failed: { target: T; reason: string }[] = [];
  const unknown: { target: T; reason: string }[] = [];
  let succeeded = 0;
  for (const outcome of outcomes) {
    if (outcome.kind === "succeeded") {
      succeeded += 1;
    } else if (outcome.kind === "failed") {
      failed.push({
        target: outcome.target,
        reason: describeFailure(outcome.error),
      });
    } else {
      unknown.push({
        target: outcome.target,
        reason: describeFailure(outcome.error),
      });
    }
  }
  return { succeeded, failed, unknown, outcomes };
}

function describeFailure(error: unknown): string {
  if (error instanceof Error && error.message.length > 0) return error.message;
  if (typeof error === "string") return error;
  return String(error);
}

/** Builds the count-gated `expectedValue` for a bulk confirmation: batches
 * above the threshold require typing the exact count. */
export function bulkExpectedCount(
  count: number,
  threshold = 5,
): string | undefined {
  return count > threshold ? String(count) : undefined;
}

/** Lists the first few targets plus a remainder line for the modal details. */
export function bulkTargetDetails(names: string[], shown = 5): string {
  if (names.length <= shown) return names.join("\n");
  return `${names.slice(0, shown).join("\n")}\nand ${
    names.length - shown
  } more`;
}
