<script lang="ts">
  import Icon from "./Icon.svelte";
  import { usesGeneratedTls } from "../config";
</script>

{#if usesGeneratedTls()}
  <p
    role="status"
    class="flex items-center justify-center gap-2 bg-warning/15 px-4 py-1.5 text-center text-[0.8125rem] text-base-content/80"
  >
    <Icon name="alert" size={14} class="text-warning" />
    This Trellis is using self-generated certificates and may not be suitable for
    production.
  </p>
{/if}
