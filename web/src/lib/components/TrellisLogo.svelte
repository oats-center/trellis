<script lang="ts">
  import logoUrl from "../../../../art/trellis-logo.svg";
  import type { ClassValue } from "svelte/elements";

  type Props = {
    class?: ClassValue;
    markClass?: ClassValue;
    logoClass?: ClassValue;
    titleClass?: ClassValue;
    subtitle?: string;
    subtitleClass?: ClassValue;
  };

  let {
    class: className,
    markClass,
    logoClass,
    titleClass,
    subtitle,
    subtitleClass,
  }: Props = $props();
</script>

<span class={["inline-flex items-center gap-3", className]}>
  <span class={["grid h-9 w-9 shrink-0 place-items-center text-secondary", markClass]}>
    <span
      class={["trellis-logo-mask h-7 w-7", logoClass]}
      style:--trellis-logo-url={`url("${logoUrl}")`}
      aria-hidden="true"
    ></span>
  </span>

  <span class="min-w-0">
    <span class={["block text-2xl font-semibold leading-none tracking-tight", titleClass]}>Trellis</span>
    {#if subtitle}
      <span class={["mt-1 block text-[10px] font-semibold uppercase tracking-[0.18em]", subtitleClass]}>{subtitle}</span>
    {/if}
  </span>
</span>
