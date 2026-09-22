<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    title: string;
    description?: string;
    actions?: Snippet;
    class?: string;
  };

  let { title, description, actions, class: className = "" }: Props = $props();
</script>

<div class={["rounded-box border border-dashed border-base-300 bg-base-100 px-4 py-8 text-center", className]}>
  <h2 class="text-sm font-semibold">{title}</h2>
  {#if description}
    <p class="mx-auto mt-2 max-w-lg text-sm text-base-content/60">{description}</p>
  {/if}
  {#if actions}
    <div class="mt-4 flex justify-center gap-2">
      {@render actions()}
    </div>
  {/if}
</div>
