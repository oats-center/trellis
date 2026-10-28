<script lang="ts">
  type Props = { status: string };

  let { status }: Props = $props();

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
</script>

<span class="inline-flex items-center gap-2 whitespace-nowrap">
  <span class={["h-2 w-2 rounded-full", view.dot]} aria-hidden="true"></span>
  {view.label}
</span>
