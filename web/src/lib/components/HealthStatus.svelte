<script lang="ts">
  type Props = {
    status: string;
    /**
     * For list rows: below the `sm` breakpoint "Healthy" shrinks to its dot
     * and only screen readers announce the word; other states keep theirs.
     */
    compact?: boolean;
  };

  let { status, compact = false }: Props = $props();

  // Health is a dot plus a word; unknown values stay neutral.
  const known: Record<string, { label: string; dot: string }> = {
    healthy: { label: "Healthy", dot: "bg-success" },
    degraded: { label: "Degraded", dot: "bg-warning" },
    unhealthy: { label: "Unhealthy", dot: "bg-error" },
    offline: { label: "Offline", dot: "bg-base-content/40" },
  };
  const view = $derived(
    known[status] ?? { label: status, dot: "bg-base-content/40" },
  );
  const shrinks = $derived(compact && status === "healthy");
</script>

<span
  class="inline-flex items-center gap-2 whitespace-nowrap"
  title={shrinks ? view.label : undefined}
>
  <span class={["h-2 w-2 shrink-0 rounded-full", view.dot]} aria-hidden="true"
  ></span>
  <span class={shrinks ? "max-sm:sr-only" : undefined}>{view.label}</span>
</span>
