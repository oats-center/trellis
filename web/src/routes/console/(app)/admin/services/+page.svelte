<script lang="ts">
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { SvelteSet } from "svelte/reactivity";

  import { resolve } from "$lib/console_paths";
  import { TABLE_PAGE_LIMIT, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import {
    nextCursorPage,
    previousCursorPage,
    resetCursorHistory,
  } from "$lib/cursor_history.ts";
  import { pruneSelection } from "$lib/bulk.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import HealthStatus from "$lib/components/HealthStatus.svelte";
  import Pager from "$lib/components/Pager.svelte";
  import { errorMessage, humanizeName } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];
  type Participant = apis.health.QueryOutput["items"][number];

  const trellis = getTrellis();
  const RPC_TIMEOUT_MS = 10_000;

  let selectedState = $state<"" | "enabled" | "disabled">("");
  let cursorHistory = $state(resetCursorHistory());
  const scope = new RequestScope("services");

  let loading = $state(true);
  let error = $state<string | null>(null);
  let deployments = $state.raw<Deployment[]>([]);
  let nextCursor = $state<string | undefined>(undefined);
  let search = $state("");
  const selected = new SvelteSet<string>();
  // Runtime health is optional context; the list stands without it.
  let healthByDeployment = $state.raw<Map<string, Participant>>(new Map());
  // Deployments whose service submitted an update that waits for approval.
  let awaitingApproval = $state.raw<Set<string>>(new Set());

  async function loadUpdates(): Promise<void> {
    const response = await trellis
      .serviceUpdatesList({ page: { limit: 200 } })
      .orThrow();
    awaitingApproval = new Set(
      response.items
        .filter((update) => update.state === "pending")
        .map((update) => update.deploymentId),
    );
  }

  async function loadHealth(): Promise<void> {
    const response = await traverseAll(async (page) => {
      const result = await trellis.healthQuery({ page }).orThrow();
      return { items: result.items, cursor: result.page.nextCursor };
    });
    // Health reports by deployment when it can, otherwise by participant name.
    const byDeployment = new Map<string, Participant>();
    for (const participant of response.items) {
      byDeployment.set(humanizeName(participant.participantName), participant);
      for (const deploymentId of participant.deploymentIds)
        byDeployment.set(deploymentId, participant);
    }
    healthByDeployment = byDeployment;
  }

  function ago(iso: string): string {
    const minutes = Math.floor((Date.now() - new Date(iso).getTime()) / 60000);
    if (!Number.isFinite(minutes)) return "—";
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    return hours < 24 ? `${hours} h ago` : `${Math.floor(hours / 24)} d ago`;
  }

  const filteredDeployments = $derived.by(() => {
    const term = search.trim().toLowerCase();
    return deployments.filter(
      (deployment) =>
        (selectedState === "" || deployment.state === selectedState) &&
        (humanizeName(deployment.title).toLowerCase().includes(term) ||
          deployment.title.toLowerCase().includes(term) ||
          deployment.deploymentId.toLowerCase().includes(term)),
    );
  });

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const response = await trellis
        .deploymentsList(
          {
            page: {
              limit: TABLE_PAGE_LIMIT,
              ...(cursorHistory.cursor === undefined
                ? {}
                : { cursor: cursorHistory.cursor }),
            },
          },
          { timeout: RPC_TIMEOUT_MS },
        )
        .take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = errorMessage(response);
        return;
      }
      deployments = (response.items ?? []).filter(
        (deployment): deployment is Deployment => deployment.kind === "service",
      );
      nextCursor = response.page.nextCursor;
      // Selection survives a same-page refresh only for rows still present.
      pruneSelection(
        selected,
        deployments.map((deployment) => deployment.deploymentId),
      );
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
    void loadHealth().catch(() => {});
    void loadUpdates().catch(() => {});
    return () => scope.invalidate();
  });

  function applyStateFilter(state: "" | "enabled" | "disabled"): void {
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

<section class="space-y-5">
  <PageToolbar title="Services" description="Services running on Trellis.">
    {#snippet actions()}
      <a class="btn btn-primary" href={resolve("/admin/services/new")}
        ><Icon name="plus" size={18} />Add service</a
      >
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error">{error}</Notice>{/if}

  <div class="trellis-section">
    <div class="flex flex-wrap items-center gap-3 p-4">
      <div class="trellis-segment" role="group" aria-label="Filter by state">
        {#each [["", "All"], ["enabled", "Active"], ["disabled", "Disabled"]] as const as [value, label] (value)}
          <button
            type="button"
            class={{ active: selectedState === value }}
            aria-pressed={selectedState === value}
            onclick={() => applyStateFilter(value)}>{label}</button
          >
        {/each}
      </div>
      <label
        class="input flex w-full max-w-md items-center gap-2 bg-base-100 sm:w-auto sm:flex-1"
      >
        <Icon name="search" size={16} class="text-base-content/50" />
        <input
          bind:value={search}
          class="grow"
          placeholder="Search services"
          aria-label="Search services"
        />
      </label>
    </div>

    {#if loading && deployments.length === 0}
      <LoadingState label="Loading services" />
    {:else if deployments.length === 0}
      <EmptyState
        class="m-4"
        title={selectedState ? "No services here" : "No services yet"}
        description={selectedState
          ? "Try a different filter."
          : "Add a service to run it on Trellis."}
      />
    {:else}
      <DataTable>
        <thead>
          <tr>
            <th>Service</th>
            <th>Health</th>
            <th>Running</th>
            <th>Version</th>
            <th>Status</th>
            <th>Last seen</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each filteredDeployments as deployment (deployment.deploymentId)}
            {@const health =
              healthByDeployment.get(deployment.deploymentId) ??
              healthByDeployment.get(humanizeName(deployment.participantId))}
            <tr>
              <td>
                <a
                  class="flex items-center gap-3"
                  href={resolve("/admin/services/[deploymentId]", {
                    deploymentId: deployment.deploymentId,
                  })}
                >
                  <span
                    class="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-base-200 text-base-content/70"
                    ><Icon name="box" size={18} /></span
                  >
                  <span class="min-w-0">
                    <span class="block truncate font-medium hover:underline"
                      >{humanizeName(deployment.title)}</span
                    >
                    {#if humanizeName(deployment.participantId) !== humanizeName(deployment.title)}
                      <span class="block truncate text-xs text-base-content/60"
                        >{humanizeName(deployment.participantId)}</span
                      >
                    {/if}
                  </span>
                </a>
              </td>
              <td>
                {#if health}<HealthStatus status={health.effectiveStatus} />
                {:else}<span class="text-base-content/50">No reports</span>{/if}
              </td>
              <td class="tabular-nums">
                {#if health}{Number(health.onlineInstances)} / {Number(
                    health.onlineInstances,
                  ) + Number(health.offlineInstances)}{:else}—{/if}
              </td>
              <td class="text-base-content/75"
                >{health?.versions.join(", ") || "—"}</td
              >
              <td>
                <span
                  class={[
                    "badge badge-sm",
                    deployment.state === "enabled"
                      ? "badge-success"
                      : "badge-neutral",
                  ]}
                  >{deployment.state === "enabled"
                    ? "Active"
                    : "Disabled"}</span
                >
                {#if awaitingApproval.has(deployment.deploymentId)}<a
                    class="badge badge-sm badge-warning ml-1"
                    href={resolve("/admin/services/[deploymentId]", {
                      deploymentId: deployment.deploymentId,
                    })}>Update waiting</a
                  >{/if}
              </td>
              <td class="text-base-content/75"
                >{health ? ago(health.lastSeenAt) : "—"}</td
              >
              <td class="text-right">
                <ActionMenu
                  widthClass="w-40"
                  ariaLabel={`Actions for ${humanizeName(deployment.title)}`}
                >
                  <li>
                    <a
                      href={resolve("/admin/services/[deploymentId]", {
                        deploymentId: deployment.deploymentId,
                      })}>Open</a
                    >
                  </li>
                </ActionMenu>
              </td>
            </tr>
          {:else}
            <tr
              ><td colspan="7" class="text-base-content/60"
                >No services match your search.</td
              ></tr
            >
          {/each}
        </tbody>
      </DataTable>
      <Pager
        summary={`Showing ${filteredDeployments.length} service${filteredDeployments.length === 1 ? "" : "s"}${nextCursor !== undefined || cursorHistory.back.length ? " on this page" : ""}`}
        canPrevious={cursorHistory.back.length > 0}
        canNext={nextCursor !== undefined}
        busy={loading}
        onPrevious={goPrevious}
        onNext={goNext}
      />
    {/if}
  </div>
</section>
