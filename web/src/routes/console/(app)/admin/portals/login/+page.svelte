<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
</script>

<section class="space-y-4">
  <PageToolbar title="Portal policy" description="Portal policy lives on each portal record; participant grant policy is in Portal grants.">
    {#snippet actions()}
      <a class="btn btn-outline btn-sm" href={resolve("/admin/services")}>Open services</a>
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Portals</a>
    {/snippet}
  </PageToolbar>

  <Panel title="Policy moved" eyebrow="Operator guidance">
    <div class="space-y-3 text-sm text-base-content/70">
      <p>
        Old default and contract-specific portal policy controls were removed. Login
        settings are edited on the portal record, and participant capability policy is
        edited in Portal grants.
      </p>
      <div class="flex flex-wrap gap-2">
        <a class="btn btn-outline btn-sm" href={resolve("/admin/portals")}>Portal records</a>
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/grants")}>Portal grants</a>
      </div>
    </div>
  </Panel>
</section>
