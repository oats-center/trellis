<script lang="ts">
  import { page } from "$app/state";
  import RoleForm from "../RoleForm.svelte";
  const roleId = $derived(page.url.searchParams.get("roleId"));
</script>

{#if roleId}<RoleForm {roleId} />{:else}<p class="p-4">
    Choose a role from the Roles table before editing.
  </p>{/if}
