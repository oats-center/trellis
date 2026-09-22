<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    title?: string;
    eyebrow?: string;
    children: Snippet;
    actions?: Snippet;
    footer?: Snippet;
    class?: string;
  };

  let { title, eyebrow, children, actions, footer, class: className = "" }: Props = $props();
</script>

<section class={["trellis-section trellis-panel", className]}>
  {#if title || eyebrow || actions}
    <div class="trellis-panel-header px-4 py-3 sm:px-5">
      <div class="flex min-h-6 items-center justify-between gap-3">
        <div class="min-w-0">
          {#if eyebrow}
            <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">{eyebrow}</p>
          {/if}
          {#if title}
            <h2 class="truncate text-base font-semibold leading-none">{title}</h2>
          {/if}
        </div>
        {#if actions}
          <div class="flex shrink-0 items-center gap-2">
            {@render actions()}
          </div>
        {/if}
      </div>
    </div>
  {/if}

  <div class="trellis-section-body p-4 sm:p-5">
    {@render children()}
  </div>

  {#if footer}
    <div class="trellis-panel-footer px-4 py-3 text-sm text-base-content/60">
      {@render footer()}
    </div>
  {/if}
</section>
