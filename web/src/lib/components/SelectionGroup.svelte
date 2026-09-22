<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";

  type Props = {
    title: string;
    description?: string;
    count?: number;
    children: Snippet;
    class?: ClassValue;
    bodyClass?: ClassValue;
  };

  let {
    title,
    description,
    count,
    children,
    class: className,
    bodyClass = "mt-4 max-h-72 overflow-y-auto rounded border border-base-300 bg-base-100/40",
  }: Props = $props();
</script>

<section class={className}>
  <div class="flex min-w-0 flex-wrap items-baseline justify-between gap-3">
    <div>
      <h3 class="trellis-field-label">{title}</h3>
      {#if description}
        <p class="trellis-field-help mt-1">{description}</p>
      {/if}
    </div>
    {#if count !== undefined}
      <span class="trellis-metadata text-xs">{count} selected</span>
    {/if}
  </div>

  <div class={bodyClass}>
    {@render children()}
  </div>
</section>
