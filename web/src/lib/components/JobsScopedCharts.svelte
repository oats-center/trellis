<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { boundedNumber, compactDuration } from "../format";

  type Props = {
    buckets: apis.jobs.MetricsOutput["buckets"];
    selectedKey?: string | null;
    windowLabel: string;
  };

  let { buckets, selectedKey = null, windowLabel }: Props = $props();

  const rows = $derived.by(() =>
    buckets.map((bucket) => {
      const groups = selectedKey ? bucket.groups.filter((group) => group.key === selectedKey) : bucket.groups;
      return {
        completed: boundedNumber(groups.reduce((sum, group) => sum + group.completed, 0n)),
        failures: boundedNumber(groups.reduce((sum, group) => sum + group.failed + group.dead, 0n)),
        queueP95: Math.max(...groups.map((group) => boundedNumber(group.queueWait.p95Ms ?? 0n)), 0),
      };
    }),
  );

  const completed = $derived(rows.reduce((sum, row) => sum + row.completed, 0));
  const failures = $derived(rows.reduce((sum, row) => sum + row.failures, 0));
  const latestQueueP95 = $derived(rows.at(-1)?.queueP95 ?? 0);
  const queueLabel = $derived(selectedKey ? "Queue wait p95" : "Highest queue p95");

  function points(values: number[]): string {
    const maximum = Math.max(...values, 1);
    const denominator = Math.max(values.length - 1, 1);
    return values.map((value, index) => `${(index / denominator) * 100},${52 - (value / maximum) * 46}`).join(" ");
  }
</script>

<aside class="jobs-trends" aria-label="Jobs trends">
  <header class="jobs-trends-header">
    <div>
      <h2>{windowLabel}</h2>
      <p>{selectedKey ?? "All job types"}</p>
    </div>
  </header>

  <div class="jobs-trend">
    <div class="jobs-trend-label"><span>Completed</span><strong>{completed.toLocaleString()}</strong></div>
    <svg viewBox="0 0 100 58" preserveAspectRatio="none" role="img" aria-label={`${completed} completed jobs over ${windowLabel.toLowerCase()}`}>
      <path d={`M0,58 L${points(rows.map((row) => row.completed))} L100,58 Z`} />
      <polyline points={points(rows.map((row) => row.completed))} />
    </svg>
  </div>

  <div class="jobs-trend queue">
    <div class="jobs-trend-label"><span>{queueLabel}</span><strong>{latestQueueP95 > 0 ? compactDuration(latestQueueP95) : "—"}</strong></div>
    <svg viewBox="0 0 100 58" preserveAspectRatio="none" role="img" aria-label={`${queueLabel} over ${windowLabel.toLowerCase()}`}>
      <polyline points={points(rows.map((row) => row.queueP95))} />
    </svg>
  </div>

  <div class="jobs-trend failures">
    <div class="jobs-trend-label"><span>Failed + dead</span><strong>{failures.toLocaleString()}</strong></div>
    <svg viewBox="0 0 100 58" preserveAspectRatio="none" role="img" aria-label={`${failures} failed or dead jobs over ${windowLabel.toLowerCase()}`}>
      <polyline points={points(rows.map((row) => row.failures))} />
    </svg>
  </div>
</aside>

<style>
  .jobs-trends {
    border-left: 1px solid color-mix(in oklab, var(--color-base-300) 85%, transparent);
    min-width: 0;
    padding-left: 1.25rem;
  }

  .jobs-trends-header {
    margin-bottom: 0.25rem;
  }

  .jobs-trends h2 {
    font-size: 0.85rem;
    font-weight: 700;
    margin: 0;
  }

  .jobs-trends p {
    color: color-mix(in oklab, var(--color-base-content) 52%, transparent);
    font-size: 0.68rem;
    margin: 0.15rem 0 0;
  }

  .jobs-trend {
    border-bottom: 1px solid color-mix(in oklab, var(--color-base-300) 75%, transparent);
    padding: 0.9rem 0 1rem;
  }

  .jobs-trend:last-child {
    border-bottom: 0;
  }

  .jobs-trend-label {
    align-items: center;
    display: flex;
    font-size: 0.68rem;
    justify-content: space-between;
  }

  .jobs-trend-label span {
    color: color-mix(in oklab, var(--color-base-content) 58%, transparent);
  }

  .jobs-trend-label strong {
    font-variant-numeric: tabular-nums;
  }

  .jobs-trend svg {
    display: block;
    height: 3.75rem;
    margin-top: 0.45rem;
    overflow: visible;
    width: 100%;
  }

  .jobs-trend path {
    fill: color-mix(in oklab, var(--color-success) 12%, transparent);
  }

  .jobs-trend polyline {
    fill: none;
    stroke: color-mix(in oklab, var(--color-success) 72%, var(--color-base-content));
    stroke-linecap: round;
    stroke-linejoin: round;
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }

  .jobs-trend.queue polyline {
    stroke: color-mix(in oklab, var(--color-primary) 75%, var(--color-base-content));
  }

  .jobs-trend.failures polyline {
    stroke: color-mix(in oklab, var(--color-error) 78%, var(--color-base-content));
  }

  .jobs-trend.failures strong {
    color: color-mix(in oklab, var(--color-error) 78%, var(--color-base-content));
  }

  @media (max-width: 980px) {
    .jobs-trends {
      border-left: 0;
      border-top: 1px solid color-mix(in oklab, var(--color-base-300) 85%, transparent);
      display: grid;
      gap: 1rem;
      grid-template-columns: repeat(3, minmax(0, 1fr));
      padding: 1rem 0 0;
    }

    .jobs-trends-header {
      grid-column: 1 / -1;
      margin: 0;
    }

    .jobs-trend {
      border-bottom: 0;
      padding: 0;
    }
  }

  @media (max-width: 640px) {
    .jobs-trends {
      grid-template-columns: 1fr;
    }

    .jobs-trends-header {
      grid-column: auto;
    }

    .jobs-trend {
      border-bottom: 1px solid color-mix(in oklab, var(--color-base-300) 75%, transparent);
      padding-bottom: 0.75rem;
    }
  }
</style>
