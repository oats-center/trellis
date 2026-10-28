<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    title: string;
    description?: string;
    actions?: Snippet;
    meta?: Snippet;
    eyebrow?: string;
    eyebrowExtra?: Snippet;
    class?: string;
  };

  let { title, description, actions, meta, eyebrow, eyebrowExtra, class: className = "" }: Props = $props();
</script>

<div class={["mb-6 flex flex-col gap-4 sm:flex-row sm:items-end sm:justify-between", className]}>
  <div class="min-w-0">
    {#if eyebrow || eyebrowExtra}
      <div class="mb-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-base-content/60">
        {#if eyebrow}
          <span class="trellis-eyebrow">{eyebrow}</span>
        {/if}
        {@render eyebrowExtra?.()}
      </div>
    {/if}
    <div class="flex flex-wrap items-center gap-2">
      <h1 class="truncate text-[2rem] font-semibold leading-tight tracking-[-0.02em] text-base-content">{title}</h1>
      {@render meta?.()}
    </div>
    {#if description}
      <p class="mt-1 max-w-3xl text-[0.95rem] text-base-content/65">{description}</p>
    {/if}
  </div>
  {#if actions}
    <div class="flex shrink-0 flex-wrap items-center gap-2">
      {@render actions()}
    </div>
  {/if}
</div>
