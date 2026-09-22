<script lang="ts">
  import { page } from "$app/state";
  import CapabilityGroupForm from "../CapabilityGroupForm.svelte";

  const targetGroupKey = $derived(page.url.searchParams.get("groupKey"));
</script>

{#key targetGroupKey}
  <CapabilityGroupForm mode="edit" {targetGroupKey} />
{/key}
