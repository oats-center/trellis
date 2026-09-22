<script lang="ts">
  type Props = {
    title: string;
    subtitle?: string;
    count?: number;
  };

  let { title, subtitle, count }: Props = $props();
</script>

<div class="sticky top-0 z-10 border-b border-base-300 bg-base-200 px-2 py-1.5">
  <div class="flex min-w-0 items-baseline justify-between gap-3">
    <div class="min-w-0">
      <div class="truncate text-xs font-semibold uppercase tracking-wide text-base-content/70">{title}</div>
      {#if subtitle}
        <div class="trellis-identifier truncate text-[0.65rem] text-base-content/50">{subtitle}</div>
      {/if}
    </div>
    {#if count !== undefined}
      <span class="trellis-metadata text-[0.65rem]">{count}</span>
    {/if}
  </div>
</div>
