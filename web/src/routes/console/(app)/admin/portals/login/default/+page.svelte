<script lang="ts">
  import { isErr } from "@qlever-llc/result";
  import { onMount } from "svelte";
  import { catalogPage } from "$lib/console/paging.ts";
  import { resolve } from "$lib/console_paths";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import PortalForm from "../../PortalForm.svelte";

  const trellis = getTrellis();
  let loading = $state(true);
  let error = $state<string | null>(null);
  /** The built-in portal ID resolved from its record, never guessed. */
  let builtInPortalId = $state<string | null>(null);

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      const response = await trellis.portalsList({ page: catalogPage() }).take();
      if (isErr(response)) {
        error = errorMessage(response);
        return;
      }
      builtInPortalId =
        response.items.find((portal) => portal.builtIn === true)?.portalId ??
          null;
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void load();
  });
</script>

<section class="space-y-4">
  <PageToolbar title="Built-in login portal" description="Login settings for the built-in portal record.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Back to portals</a>
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error">{error}</Notice>{/if}

  {#if loading}
    <Panel><LoadingState label="Loading built-in portal" /></Panel>
  {:else if builtInPortalId === null}
    <EmptyState
      title="Built-in portal unavailable"
      description="No built-in portal record was returned. It cannot be removed, only configured."
      class="m-5"
    />
  {:else}
    <PortalForm mode="edit" targetPortalId={builtInPortalId} />
  {/if}
</section>
