<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";

  type TableSize = "sm" | "xs";
  type TableOverflow = "auto" | "visible" | "none";

  type Props = {
    children: Snippet;
    class?: ClassValue;
    tableClass?: ClassValue;
    wrapperClass?: ClassValue;
    size?: TableSize;
    fixed?: boolean;
    overflow?: TableOverflow;
  };

  let {
    children,
    class: className,
    tableClass,
    wrapperClass,
    size = "sm",
    fixed = false,
    overflow = "auto",
  }: Props = $props();
</script>

{#snippet table()}
  <table class={[`table table-${size} trellis-table`, fixed && "table-fixed", className, tableClass]}>
    {@render children()}
  </table>
{/snippet}

{#if overflow === "none"}
  {@render table()}
{:else}
  <div class={[overflow === "auto" ? "overflow-x-auto" : "overflow-visible", wrapperClass]}>
    {@render table()}
  </div>
{/if}
