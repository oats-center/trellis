<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
</script>

<section class="space-y-4">
  <PageToolbar title="Portal routes" description="Route rules are scoped to each portal record and select on participant, deployment, and origin.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Back to portals</a>
    {/snippet}
  </PageToolbar>

  <Panel title="Manage routes from a portal" eyebrow="Participant/deployment/origin selectors">
    <div class="flex flex-wrap items-center justify-between gap-3 px-4 py-3 text-sm">
      <p class="text-base-content/60">
        Open a portal record, then add or edit its participant, deployment, and origin
        selectors from the Portal routes panel. A route with no selectors is an
        intentional catch-all and requires explicit confirmation.
      </p>
      <a class="btn btn-outline btn-sm" href={resolve("/admin/portals")}>View portals</a>
    </div>
  </Panel>
</section>
