<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
</script>

<section class="space-y-4">
  <PageToolbar title="Device portal policy" description="Device portal routing is now deployment permission metadata.">
    {#snippet actions()}
      <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Open devices</a>
    {/snippet}
  </PageToolbar>

  <Panel title="Policy moved" eyebrow="Operator guidance">
    <div class="space-y-3 text-sm text-base-content/70">
      <p>Default and deployment-specific device portal APIs have been removed. Use device deployments to inspect activation routes and authority metadata.</p>
      <div class="flex flex-wrap gap-2">
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Inspect devices</a>
      </div>
    </div>
  </Panel>
</section>
