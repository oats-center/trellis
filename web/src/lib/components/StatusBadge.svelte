<script lang="ts">
  type StatusVariant = "healthy" | "degraded" | "unhealthy" | "offline";

  type Props = {
    label: string;
    status?: StatusVariant;
    class?: string;
  };

  let { label, status = "offline", class: className = "" }: Props = $props();

  const badgeClass = $derived(
    status === "healthy" ? "badge-success" :
      status === "degraded" ? "badge-warning" :
        status === "unhealthy" ? "badge-error" : "badge-neutral"
  );
</script>

<span class={["badge badge-sm trellis-badge-soft border-0 font-semibold", badgeClass, className]}>{label}</span>
