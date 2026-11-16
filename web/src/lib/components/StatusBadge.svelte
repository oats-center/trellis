<script lang="ts">
  type StatusVariant = "healthy" | "degraded" | "unhealthy" | "offline";

  type Props = {
    label: string;
    status?: StatusVariant;
    /** Leads the label with a coloured dot. */
    dot?: boolean;
    /**
     * For list rows: below the `sm` breakpoint the ordinary state shrinks to
     * a coloured dot whose label only screen readers announce, while every
     * other state keeps its word.
     */
    compact?: boolean;
    /** Whether this is the ordinary state; defaults to `healthy`. */
    quiet?: boolean;
    /**
     * Whether the shrunk ordinary state keeps its dot. Without it the state
     * is announced but not drawn, for rows that already show a health dot.
     */
    quietDot?: boolean;
    class?: string;
  };

  let {
    label,
    status = "offline",
    dot = false,
    compact = false,
    quiet,
    quietDot = true,
    class: className = "",
  }: Props = $props();

  const badgeClass = $derived(
    status === "healthy" ? "badge-success" :
      status === "degraded" ? "badge-warning" :
        status === "unhealthy" ? "badge-error" : "badge-neutral"
  );
  const dotClass = $derived(
    status === "healthy" ? "bg-success" :
      status === "degraded" ? "bg-warning" :
        status === "unhealthy" ? "bg-error" : "bg-base-content/40"
  );
  const shrinks = $derived(compact && (quiet ?? status === "healthy"));
</script>

<span
  class={["badge badge-sm trellis-badge-soft border-0 font-semibold", (dot || shrinks) && "gap-1.5", badgeClass, className]}
  class:shrinks
  title={shrinks ? label : undefined}
  >{#if dot || (shrinks && quietDot)}<span
      class={["h-1.5 w-1.5 shrink-0 rounded-full", dotClass, !dot && "sm:hidden"]}
      aria-hidden="true"
    ></span>{/if}<span class={shrinks ? "max-sm:sr-only" : undefined}>{label}</span
  ></span
>

<style>
  @media (max-width: 39.99rem) {
    .badge.shrinks:not(.badge-outline) {
      background: transparent;
      padding-inline: 0.25rem;
    }

    .badge.shrinks:not(:has(> [aria-hidden])) {
      padding-inline: 0;
    }

    .badge.shrinks > [aria-hidden] {
      height: 0.5rem;
      width: 0.5rem;
    }
  }
</style>
