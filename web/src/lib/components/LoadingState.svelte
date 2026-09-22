<script lang="ts">
  type Props = {
    label?: string;
    class?: string;
  };

  let { label = "Loading", class: className = "" }: Props = $props();
</script>

<div class={["flex min-h-48 items-center justify-center gap-3 text-sm text-base-content/60", className]} role="status" aria-live="polite">
  <span class="loading loading-spinner loading-sm"></span>
  <span>{label}</span>
</div>
