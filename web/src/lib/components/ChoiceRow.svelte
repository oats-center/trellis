<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";

  type Props = {
    children: Snippet;
    input: Snippet;
    class?: ClassValue;
    compact?: boolean;
  };

  let { children, input, class: className, compact = false }: Props = $props();
</script>

<label
  class={[
    "grid cursor-pointer grid-cols-[auto_1fr] gap-2 border-b border-base-300/70 text-xs last:border-b-0 hover:bg-base-200/60",
    compact ? "py-2" : "px-2 py-2",
    className,
  ]}
>
  {@render input()}
  <span class="min-w-0">
    {@render children()}
  </span>
</label>
