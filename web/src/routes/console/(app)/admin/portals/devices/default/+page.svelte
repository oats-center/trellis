<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
</script>

<section class="space-y-4">
  <PageToolbar title="Default device portal" description="Device onboarding is configured on device deployments and portal records.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals/devices")}>Back to device guidance</a>
    {/snippet}
  </PageToolbar>

  <Panel title="No standalone default" eyebrow="Operator guidance">
    <div class="space-y-3 px-4 py-3 text-sm text-base-content/70">
      <p>
        There is no standalone default-device-portal setting. Device onboarding is
        expressed by device deployments and by the route selectors on portal records;
        this stabilization does not add a separate deployment-portal policy API.
      </p>
      <div class="flex flex-wrap gap-2">
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Device deployments</a>
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Portal records</a>
      </div>
    </div>
  </Panel>
</section>
