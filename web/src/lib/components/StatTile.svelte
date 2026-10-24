<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import Icon from "./Icon.svelte";
  import Sparkline from "./Sparkline.svelte";

  type Tone = "good" | "warning" | "bad" | "neutral";

  type Props = {
    label: string;
    icon: string;
    value: string;
    detail?: string;
    tone?: Tone;
    trend?: readonly number[];
    href?: string;
  };

  let {
    label,
    icon,
    value,
    detail,
    tone = "neutral",
    trend = [],
    href,
  }: Props = $props();

  const dot = $derived(
    {
      good: "bg-success",
      warning: "bg-warning",
      bad: "bg-error",
      neutral: "",
    }[tone],
  );
</script>

{#snippet body()}
  <span
    class="grid h-11 w-11 shrink-0 place-items-center rounded-full bg-base-200 text-base-content/70"
    ><Icon name={icon} size={20} /></span
  >
  <span class="min-w-0 flex-1">
    <span class="block truncate text-sm text-base-content/70">{label}</span>
    <span
      class="block truncate text-[1.75rem] font-semibold leading-tight tracking-[-0.02em]"
      >{value}</span
    >
    <span class="mt-1 flex items-end justify-between gap-3">
      <span
        class="flex min-w-0 items-center gap-1.5 text-[0.8125rem] text-base-content/65"
      >
        {#if dot}<span class={["h-2 w-2 shrink-0 rounded-full", dot]}
          ></span>{/if}
        <span class="truncate">{detail ?? ""}</span>
      </span>
      {#if trend.length > 1}<Sparkline
          values={trend}
          class="h-8 w-24 shrink-0"
        />{/if}
    </span>
  </span>
{/snippet}

{#if href}
  <a
    href={resolve(href)}
    class="flex items-start gap-4 rounded-box border border-base-300 bg-base-100 p-5 transition-colors hover:border-base-content/25"
    >{@render body()}</a
  >
{:else}
  <div
    class="flex items-start gap-4 rounded-box border border-base-300 bg-base-100 p-5"
  >
    {@render body()}
  </div>
{/if}
