<script lang="ts">
  import Notice from "./Notice.svelte";
  import { usesGeneratedTls } from "../config";
</script>

{#if usesGeneratedTls()}
  <Notice variant="warning" role="status" class="rounded-none text-sm">
    This Trellis is using self-generated certificates and may not be suitable
    for production.
  </Notice>
{/if}
