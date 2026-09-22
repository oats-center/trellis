<script lang="ts">
  type Metric = {
    label: string;
    value: string | number;
    detail?: string;
    badge?: string;
    badgeClass?: string;
    dot?: "success" | "warning" | "error" | "info" | "neutral";
  };

  type Props = {
    metrics: Metric[];
    class?: string;
  };

  let { metrics, class: className = "" }: Props = $props();
</script>

<dl class={["trellis-surface grid overflow-hidden bg-base-100 sm:grid-cols-2 lg:grid-cols-5", className]}>
  {#each metrics as metric (metric.label)}
    <div class="flex min-h-16 items-center border-b border-base-300 px-5 text-sm last:border-b-0 sm:[&:nth-last-child(-n+2)]:border-b-0 lg:border-b-0 lg:border-r lg:last:border-r-0">
      {#if metric.dot}
        <span class={["trellis-dot mr-3", metric.dot]}></span>
      {/if}
      <dt class="font-medium leading-tight text-base-content/80">{metric.label}</dt>
      <dd class="ml-2 font-semibold tabular-nums">{metric.value}</dd>
      {#if metric.detail}
        <span class="ml-1 text-base-content/55">{metric.detail}</span>
      {/if}
      {#if metric.badge}
        <span class={["badge badge-sm trellis-badge-soft ml-2 border-0", metric.badgeClass ?? "badge-neutral"]}>{metric.badge}</span>
      {/if}
    </div>
  {/each}
</dl>
