<script lang="ts">
  type Point = { label: string; tick: string; value: number };
  type Props = {
    points: readonly Point[];
    /** Names the plotted measure for the tooltip and screen readers. */
    unit: string;
  };

  let { points, unit }: Props = $props();

  const HEIGHT = 180;
  const AXIS = 36;
  const BOTTOM = 22;
  let width = $state(600);
  let hover = $state<number | null>(null);

  const compact = new Intl.NumberFormat(undefined, { notation: "compact" });

  // Round the top of the scale to a clean number so ticks read naturally.
  const scaleMax = $derived.by(() => {
    const max = Math.max(1, ...points.map((point) => point.value));
    const magnitude = 10 ** Math.floor(Math.log10(max));
    return Math.ceil(max / magnitude) * magnitude;
  });
  const plot = $derived({
    left: AXIS,
    width: Math.max(width - AXIS, 1),
    height: HEIGHT - BOTTOM,
  });
  const slot = $derived(plot.width / Math.max(points.length, 1));
  const bar = $derived(Math.max(2, Math.min(24, slot - 2)));
  const ticks = $derived([0, scaleMax / 2, scaleMax]);
  // Label a handful of x positions, never every bar.
  const labelEvery = $derived(Math.max(1, Math.ceil(points.length / 6)));

  function y(value: number): number {
    return plot.height - (value / scaleMax) * (plot.height - 8);
  }

  function columnPath(x: number, top: number): string {
    const radius = Math.min(4, bar / 2, plot.height - top);
    const base = plot.height;
    return `M${x},${base}V${top + radius}Q${x},${top} ${x + radius},${top}H${x + bar - radius}Q${x + bar},${top} ${x + bar},${top + radius}V${base}Z`;
  }
</script>

<div class="relative" bind:clientWidth={width}>
  <svg
    width="100%"
    height={HEIGHT}
    role="img"
    aria-label={`${unit} over time`}
    onmouseleave={() => (hover = null)}
  >
    {#each ticks as tick (tick)}
      <line
        x1={plot.left}
        x2={plot.left + plot.width}
        y1={y(tick)}
        y2={y(tick)}
        class="stroke-base-300"
        stroke-width="1"
      />
      <text
        x={plot.left - 8}
        y={y(tick)}
        dy="0.32em"
        text-anchor="end"
        class="fill-base-content/55 text-[11px]">{compact.format(tick)}</text
      >
    {/each}
    {#each points as point, index (point.label)}
      {@const x = plot.left + index * slot + (slot - bar) / 2}
      {#if point.value > 0}
        <path
          d={columnPath(x, y(point.value))}
          class={hover === index ? "fill-primary" : "fill-primary/75"}
        />
      {/if}
      {#if index % labelEvery === 0}
        <text
          x={x + bar / 2}
          y={HEIGHT - 6}
          text-anchor="middle"
          class="fill-base-content/55 text-[11px]">{point.tick}</text
        >
      {/if}
      <rect
        role="presentation"
        x={plot.left + index * slot}
        y="0"
        width={slot}
        height={plot.height}
        fill="transparent"
        onmouseenter={() => (hover = index)}
      />
    {/each}
  </svg>
  <table class="sr-only">
    <caption>{unit} over time</caption>
    <tbody>
      {#each points as point (point.label)}
        <tr><th scope="row">{point.label}</th><td>{point.value}</td></tr>
      {/each}
    </tbody>
  </table>
  {#if hover !== null && points[hover]}
    {@const point = points[hover]}
    {@const left = plot.left + hover * slot + slot / 2}
    <div
      class="pointer-events-none absolute top-0 z-10 -translate-x-1/2 whitespace-nowrap rounded-field border border-base-300 bg-base-100 px-2.5 py-1.5 text-xs"
      style:left={`${Math.min(Math.max(left, 60), width - 60)}px`}
    >
      <div class="text-base-content/60">{point.label}</div>
      <div class="font-medium">{point.value.toLocaleString()} {unit}</div>
    </div>
  {/if}
</div>
