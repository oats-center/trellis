<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";

  type MenuAlign = "start" | "end";

  type Props = {
    children: Snippet;
    summary?: Snippet;
    label?: string;
    class?: ClassValue;
    buttonBaseClass?: ClassValue;
    buttonClass?: ClassValue;
    menuClass?: ClassValue;
    widthClass?: ClassValue;
    align?: MenuAlign;
    ariaLabel?: string;
    dataActionMenu?: boolean;
  };

  let {
    children,
    summary,
    label = "Actions",
    class: className,
    buttonBaseClass = "btn btn-ghost btn-xs",
    buttonClass,
    menuClass,
    widthClass = "w-48",
    align = "end",
    ariaLabel,
    dataActionMenu = false,
  }: Props = $props();
</script>

<details
  class={["dropdown", align === "end" ? "dropdown-end" : "dropdown-start", className]}
  data-action-menu={dataActionMenu ? "" : undefined}
>
  <summary class={[buttonBaseClass, buttonClass]} aria-label={ariaLabel}>
    {#if summary}
      {@render summary()}
    {:else}
      {label}
    {/if}
  </summary>
  <ul class={["menu dropdown-content trellis-dropdown-menu z-30 mt-2 rounded-box border border-base-300 bg-base-100 p-2", widthClass, menuClass]}>
    {@render children()}
  </ul>
</details>
