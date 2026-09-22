<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { boundedNumber, compactDuration } from "../format";

  type SummaryGroup = apis.jobs.MetricsOutput["summary"][number];
  type Bucket = apis.jobs.MetricsOutput["buckets"][number];

  type Props = {
    summary: SummaryGroup[];
    buckets: Bucket[];
    selectedKey?: string | null;
    onSelect?: (key: string | null) => void;
  };

  let { summary, buckets, selectedKey = null, onSelect }: Props = $props();

  const sparklineByKey = $derived.by(() => {
    const values: Record<string, number[]> = {};
    for (const bucket of buckets) {
      for (const group of bucket.groups) {
        const failures = boundedNumber(group.failed + group.dead + group.retried);
        values[group.key] = [...(values[group.key] ?? []), failures];
      }
    }
    return values;
  });

  const sortedSummary = $derived.by(() =>
    [...summary].sort((left, right) => {
      const leftPressure = boundedNumber(left.failed ?? 0n) + boundedNumber(left.dead ?? 0n) * 2 + boundedNumber(left.queued ?? 0n) / 20;
      const rightPressure = boundedNumber(right.failed ?? 0n) + boundedNumber(right.dead ?? 0n) * 2 + boundedNumber(right.queued ?? 0n) / 20;
      return rightPressure - leftPressure || boundedNumber(right.total - left.total);
    }),
  );

  function severity(group: SummaryGroup): string {
    const failures = (group.failed ?? 0n) + (group.dead ?? 0n);
    if (failures > 0n) return "danger";
    if ((group.queued ?? 0n) > 0n || (group.slow ?? 0n) > 0n) return "warning";
    return "healthy";
  }

  function formatMs(value: bigint | undefined): string {
    return value === undefined ? "—" : compactDuration(boundedNumber(value));
  }

  function sparkPoints(values: number[]): string {
    const maximum = Math.max(...values, 1);
    const denominator = Math.max(values.length - 1, 1);
    return values.map((value, index) => `${(index / denominator) * 100},${19 - (value / maximum) * 17}`).join(" ");
  }

  function select(key: string) {
    onSelect?.(selectedKey === key ? null : key);
  }

  function handleKey(event: KeyboardEvent, key: string) {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    select(key);
  }
</script>

<div class="jobs-matrix" role="table" aria-label="Job type health">
  <div class="jobs-matrix-row jobs-matrix-head" role="row">
    <span role="columnheader">Job type</span>
    <span role="columnheader">Backlog</span>
    <span role="columnheader">Running</span>
    <span role="columnheader">Failed</span>
    <span role="columnheader">Dead</span>
    <span role="columnheader">Queue p95</span>
    <span role="columnheader">Runtime p95</span>
    <span role="columnheader">Failure trend</span>
  </div>

  {#each sortedSummary as group (group.key)}
    {@const spark = sparklineByKey[group.key] ?? []}
    <div
      class={["jobs-matrix-row", selectedKey === group.key && "selected"]}
      role="row"
      tabindex="0"
      aria-label={`${group.label}: ${group.queued ?? 0} queued, ${group.running ?? 0} running, ${group.failed ?? 0} failed, ${group.dead ?? 0} dead`}
      onclick={() => select(group.key)}
      onkeydown={(event) => handleKey(event, group.key)}
    >
      <span role="cell" class="jobs-matrix-type">
        <span class="jobs-matrix-key">{group.label}</span>
        <span class={["badge badge-sm trellis-badge-soft border-0 font-semibold", severity(group) === "danger" ? "badge-error" : severity(group) === "warning" ? "badge-warning" : "badge-success"]}>
          {group.failureRate === undefined ? "No completions" : `${(group.failureRate * 100).toFixed(1)}% failed`}
        </span>
      </span>
      <span role="cell" class:warning={(group.queued ?? 0) > 0} data-label="Backlog">{group.queued ?? 0}</span>
      <span role="cell" data-label="Running">{group.running ?? 0}</span>
      <span role="cell" class:danger={(group.failed ?? 0) > 0} data-label="Failed">{group.failed ?? 0}</span>
      <span role="cell" class:danger={(group.dead ?? 0) > 0} data-label="Dead">{group.dead ?? 0}</span>
      <span role="cell" class="latency" data-label="Queue p95">{formatMs(group.queueWait.p95Ms)}</span>
      <span role="cell" class="latency" data-label="Runtime p95">{formatMs(group.runtime.p95Ms)}</span>
      <span role="cell" class="jobs-sparkline" aria-label="Recent failure trend">
        {#if spark.length > 0}
          <svg viewBox="0 0 100 20" preserveAspectRatio="none" aria-hidden="true">
            <polyline points={sparkPoints(spark)} />
          </svg>
        {:else}
          <span>no data</span>
        {/if}
      </span>
    </div>
  {:else}
    <p class="jobs-matrix-empty">No job types reported activity in this window.</p>
  {/each}
</div>

<style>
  .jobs-matrix {
    border-top: 1px solid color-mix(in oklab, var(--color-base-300) 85%, transparent);
    min-width: 0;
  }

  .jobs-matrix-row {
    align-items: center;
    border-bottom: 1px solid color-mix(in oklab, var(--color-base-300) 75%, transparent);
    cursor: pointer;
    display: grid;
    font-size: 0.75rem;
    gap: 0.75rem;
    grid-template-columns: minmax(10rem, 2fr) repeat(6, minmax(3.5rem, 0.72fr)) minmax(5.5rem, 1fr);
    min-height: 3rem;
    padding: 0.35rem 0.5rem;
    transition: background-color 150ms cubic-bezier(0.22, 1, 0.36, 1);
  }

  .jobs-matrix-row:hover {
    background: color-mix(in oklab, var(--color-base-200) 55%, transparent);
  }

  .jobs-matrix-row.selected {
    background: color-mix(in oklab, var(--color-primary) 10%, var(--color-base-100));
    box-shadow: inset 0 -2px color-mix(in oklab, var(--color-primary) 70%, var(--color-base-content));
  }

  .jobs-matrix-row:focus-visible {
    box-shadow: var(--trellis-focus-ring);
    outline: none;
  }

  .jobs-matrix-head {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    cursor: default;
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.07em;
    min-height: 2.25rem;
    text-transform: uppercase;
  }

  .jobs-matrix-head:hover {
    background: transparent;
  }

  .jobs-matrix-head span:not(:first-child),
  .jobs-matrix-row > span:not(:first-child) {
    text-align: right;
  }

  .jobs-matrix-type {
    align-items: center;
    display: flex;
    gap: 0.5rem;
    min-width: 0;
  }

  .jobs-matrix-key {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-weight: 650;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .danger {
    color: color-mix(in oklab, var(--color-error) 78%, var(--color-base-content));
    font-weight: 700;
  }

  .warning {
    color: color-mix(in oklab, var(--color-warning) 62%, var(--color-base-content));
    font-weight: 700;
  }

  .latency {
    color: color-mix(in oklab, var(--color-base-content) 68%, transparent);
    font-variant-numeric: tabular-nums;
  }

  .jobs-sparkline {
    align-items: center;
    color: color-mix(in oklab, var(--color-base-content) 58%, transparent);
    display: flex;
    height: 1.35rem;
    justify-content: flex-end;
  }

  .jobs-sparkline svg {
    height: 100%;
    width: 100%;
  }

  .jobs-sparkline polyline {
    fill: none;
    stroke: color-mix(in oklab, var(--color-error) 75%, var(--color-base-content));
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }

  .jobs-matrix-empty {
    color: color-mix(in oklab, var(--color-base-content) 66%, transparent);
    font-size: 0.78rem;
    margin: 0;
    padding: 1.5rem 0.5rem;
  }

  @media (max-width: 760px) {
    .jobs-matrix-head {
      display: none;
    }

    .jobs-matrix-row {
      grid-template-columns: repeat(4, minmax(0, 1fr));
      min-height: auto;
      padding: 0.8rem 0.25rem;
      row-gap: 0.7rem;
    }

    .jobs-matrix-type {
      grid-column: 1 / -1;
      justify-content: space-between;
    }

    .jobs-matrix-row > span:not(:first-child) {
      display: grid;
      gap: 0.2rem;
      text-align: left;
    }

    .jobs-matrix-row > span:not(:first-child)::before {
      color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
      content: attr(data-label);
      display: block;
      font-size: 0.68rem;
      font-weight: 700;
      letter-spacing: 0.06em;
      text-transform: uppercase;
    }

    .jobs-matrix-row .latency,
    .jobs-matrix-row .jobs-sparkline {
      display: none;
    }
  }
</style>
