<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
</script>

<section class="space-y-4">
  <PageToolbar title="Device portal selection" description="Device portal selection is expressed by portal route selectors.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals/devices")}>Back to device guidance</a>
    {/snippet}
  </PageToolbar>

  <Panel title="Selections moved" eyebrow="Operator guidance">
    <div class="space-y-3 px-4 py-3 text-sm text-base-content/70">
      <p>
        Standalone device-portal selection settings were removed. The effective
        selection now comes from each portal record's participant, deployment, and
        origin route selectors.
      </p>
      <div class="flex flex-wrap gap-2">
        <a class="btn btn-outline btn-sm" href={resolve("/admin/portals")}>Portal records</a>
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Device deployments</a>
      </div>
    </div>
  </Panel>
</section>
