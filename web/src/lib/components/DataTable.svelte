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

<style>
  /*
   * Below the `sm` breakpoint a row gives its width to the identifying text.
   * The cell marked `trellis-primary` takes whatever the other visible cells
   * leave, and its text wraps instead of truncating. Pages hide secondary
   * columns there (`hidden sm:table-cell`), repeat what still matters in a
   * muted line under the name, and shrink status beside the name.
   */
  @media (max-width: 39.99rem) {
    .trellis-table {
      min-width: 0;
      width: 100%;
    }

    .trellis-table :global(:is(th, td):not(.trellis-primary)) {
      padding-inline: 0.375rem;
    }

    .trellis-table :global(td.trellis-primary) {
      max-width: 0;
      overflow-wrap: anywhere;
      white-space: normal;
      width: 100%;
    }

    .trellis-table :global(td.trellis-primary .truncate) {
      overflow: visible;
      text-overflow: clip;
      white-space: normal;
    }

    .trellis-table :global(td.trellis-primary .trellis-identifier) {
      max-width: 100%;
    }
  }
</style>
