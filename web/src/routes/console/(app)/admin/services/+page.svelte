<script lang="ts">
  import { isErr } from "@oats-center/result";
  import { type apis } from "trellis-web-generated";
  import { SvelteSet } from "svelte/reactivity";

  import { resolve } from "$lib/console_paths";
  import { TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { nextCursorPage, previousCursorPage, resetCursorHistory } from "$lib/cursor_history.ts";
  import { pruneSelection } from "$lib/bulk.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];

  const trellis = getTrellis();
  const RPC_TIMEOUT_MS = 10_000;

  let selectedState = $state<"" | "active" | "disabled" | "revoked">("");
  let cursorHistory = $state(resetCursorHistory());
  const scope = new RequestScope("services");

  let loading = $state(true);
  let error = $state<string | null>(null);
  let deployments = $state.raw<Deployment[]>([]);
  let nextCursor = $state<string | undefined>(undefined);
  let search = $state("");
  const selected = new SvelteSet<string>();

  const filteredDeployments = $derived.by(() => {
    const term = search.trim().toLowerCase();
    if (!term) return deployments;
    return deployments.filter((deployment) =>
      deployment.displayName.toLowerCase().includes(term) || deployment.deploymentId.toLowerCase().includes(term)

    );
  });
  const disabledCount = $derived(
    deployments.filter((deployment) => deployment.state !== "active").length,
  );

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const response = await trellis.deploymentsList({
        kind: "service",
        ...(selectedState === "" ? {} : { state: selectedState }),
        page: {
          limit: TABLE_PAGE_LIMIT,
          ...(cursorHistory.cursor === undefined ? {} : { cursor: cursorHistory.cursor }),
        },
      }, { timeout: RPC_TIMEOUT_MS }).take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = errorMessage(response);
        return;
      }
      deployments = (response.items ?? [])
        .filter((deployment): deployment is Deployment => deployment.kind === "service");
      nextCursor = response.page.nextCursor;
      // Selection survives a same-page refresh only for rows still present.
      pruneSelection(selected, deployments.map((deployment) => deployment.deploymentId));
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = errorMessage(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  // A server filter change resets the cursor; cursor history drives paging.
  $effect(() => {
    void selectedState;
    scope.setKey(`services:${selectedState}`);
    void load();
    return () => scope.invalidate();
  });

  function applyStateFilter(state: "" | "active" | "disabled" | "revoked"): void {
    if (state === selectedState) return;
    selectedState = state;
    cursorHistory = resetCursorHistory();
    selected.clear();
  }

  function goNext(): void {
    if (nextCursor === undefined) return;
    const previous = cursorHistory;
    cursorHistory = nextCursorPage(cursorHistory, nextCursor);
    void load().then(() => {
      // A failed page keeps the previous cursor and data visible.
      if (error !== null) cursorHistory = previous;
    });
  }

  function goPrevious(): void {
    cursorHistory = previousCursorPage(cursorHistory);
    void load();
  }
</script>

<section class="space-y-4">
  <PageToolbar title="Service runtime" description="Service deployments and their participant-scoped authority.">
    {#snippet actions()}
      <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading}>Refresh</button>
      <a class="btn btn-outline btn-sm" href={resolve("/admin/services/new")}>Create service</a>
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error">{error}</Notice>{/if}

  {#if loading && deployments.length === 0}
    <Panel><LoadingState label="Loading services" /></Panel>
  {:else}
    <Panel>
      <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
        <div>
          <h2 class="text-sm font-semibold uppercase tracking-wide text-base-content/70">Service fleet</h2>
          <p class="text-xs text-base-content/50">
            {deployments.length} on this page · {disabledCount} not active · page size {TABLE_PAGE_LIMIT}
          </p>
        </div>
        <div class="flex flex-wrap items-center gap-2">
          <label class="input input-bordered input-sm flex items-center gap-2">
            <Icon name="search" size={14} class="text-base-content/50" />
            <input bind:value={search} class="grow" placeholder="Search this page" aria-label="Search deployments" />
          </label>
          <label class="select select-bordered select-sm">
            <select
              aria-label="Filter by state"
              value={selectedState}
              onchange={(event) => applyStateFilter(event.currentTarget.value as "" | "active" | "disabled" | "revoked")}
            >
              <option value="">All states</option>
              <option value="active">Active</option>
              <option value="disabled">Disabled</option>
              <option value="revoked">Revoked</option>
            </select>
          </label>
        </div>
      </div>

      {#if deployments.length === 0}
        <EmptyState title="No service deployments" description="Create a deployment profile to inspect and install a service participant." />
      {:else}
        <DataTable>
          <thead><tr><th>Deployment</th><th>State</th><th>Participant</th><th>Version</th><th>Updated</th></tr></thead>
          <tbody>
            {#each filteredDeployments as deployment (deployment.deploymentId)}
              <tr class="hover:bg-base-200/60">
                <td class="min-w-72">
                  <a
                    class="btn btn-ghost h-auto min-h-0 justify-start gap-2 px-2 py-1 text-left"
                    href={resolve("/admin/services/[deploymentId]", { deploymentId: deployment.deploymentId })}
                  >
                    <span class="h-2.5 w-2.5 rounded-full {deployment.state === "active" ? "bg-success" : "bg-base-content/30"}"></span>
                    <span class="min-w-0">
                      <span class="block truncate font-medium">{deployment.displayName}</span>
                      <span class="trellis-identifier block truncate text-xs text-base-content/50">{deployment.deploymentId}</span>
                    </span>
                  </a>
                </td>
                <td>
                  <span class="badge badge-sm {deployment.state === "active" ? "badge-success" : "badge-neutral"}">{deployment.state}</span>
                </td>
                <td class="trellis-identifier truncate text-xs">{deployment.participantId ?? "Not installed"}</td>
                <td>{deployment.version}</td>
                <td class="text-base-content/60">{deployment.updatedAt}</td>
              </tr>
            {:else}
              <tr><td colspan="5" class="text-base-content/50">No matching deployments on this page.</td></tr>
            {/each}
          </tbody>
        </DataTable>

        <div class="mt-3 flex items-center justify-between gap-2">
          <span class="text-xs text-base-content/50">{deployments.length} on this page</span>
          <div class="join">
            <button class="btn btn-sm join-item" onclick={goPrevious} disabled={cursorHistory.back.length === 0 || loading}>
              Previous
            </button>
            <button class="btn btn-sm join-item" onclick={goNext} disabled={nextCursor === undefined || loading}>
              Next
            </button>
          </div>
        </div>
      {/if}
    </Panel>
  {/if}
</section>
