<script lang="ts">
  type Props = { values: readonly number[]; class?: string };

  let { values, class: className = "" }: Props = $props();

  const WIDTH = 120;
  const HEIGHT = 36;
  const PAD = 3;

  // Single-series trend: a 2px line over a 10% wash, scaled to its own range.
  const path = $derived.by(() => {
    if (values.length < 2) return null;
    const max = Math.max(...values);
    const min = Math.min(...values);
    const span = max - min || 1;
    const points = values.map((value, index) => [
      (index / (values.length - 1)) * WIDTH,
      HEIGHT - PAD - ((value - min) / span) * (HEIGHT - PAD * 2),
    ]);
    const line = points
      .map(
        ([x, y], index) =>
          `${index ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)}`,
      )
      .join("");
    return { line, area: `${line}L${WIDTH},${HEIGHT}L0,${HEIGHT}Z` };
  });
</script>

{#if path}
  <svg
    class={["text-primary", className]}
    viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
    preserveAspectRatio="none"
    aria-hidden="true"
  >
    <path d={path.area} fill="currentColor" opacity="0.1" />
    <path
      d={path.line}
      fill="none"
      stroke="currentColor"
      stroke-width="2"
      stroke-linejoin="round"
      stroke-linecap="round"
      vector-effect="non-scaling-stroke"
    />
  </svg>
{/if}
