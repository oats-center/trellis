<script lang="ts">
  import { page } from "$app/state";
  import PortalForm from "../PortalForm.svelte";

  const targetPortalId = $derived(page.url.searchParams.get("portalId"));
</script>

{#key targetPortalId}
  <PortalForm mode="edit" {targetPortalId} />
{/key}
