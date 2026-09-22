<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import { formatDateTime, formatRelativeAge } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { workflowQuery } from "$lib/workflow";

  type InspectionAssignment = {
    inspectionId: string;
    siteId: string;
    siteName: string;
    assetName: string;
    checklistName: string;
    priority: string;
  };
  type SiteSummary = {
    siteId?: string;
    siteName: string;
    latestStatus: string;
    openInspections: number;
    overdueInspections: number;
    lastReportAt: string;
  };
  type SitesRefreshProgress = { stage: string; message: string };
  type SitesRefreshResponse = { refreshId: string; site: SiteSummary; status: string };
  type RefreshEvent = {
    type: string;
    snapshot: { state: string; output?: SitesRefreshResponse };
    progress?: SitesRefreshProgress;
  };
  type RefreshTerminal = { state: string; output?: SitesRefreshResponse };
  type RefreshOperationRef = {
    id: string;
    watch(): { orThrow(): Promise<AsyncIterable<RefreshEvent>> };
    wait(): { orThrow(): Promise<RefreshTerminal> };
  };
  type LocalOperationUpdate = {
    kind: "operation";
    id: string;
    operationId: string;
    name: "Sites.Refresh";
    action: string;
    subject: string;
    state: string;
    occurredAt: string;
    refreshId?: string;
  };

  const trellis = getTrellis();

  type InspectionRoute = "/inspection" | `/inspection?${string}`;
  type EvidenceRoute = "/evidence" | `/evidence?${string}`;
  const listPage = { page: { limit: 50 } };

  let loadingAssignments = $state(true);
  let loadingSites = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let assignments = $state<InspectionAssignment[]>([]);
  let sites = $state<SiteSummary[]>([]);
  let selectedSiteId = $state<string | null>(null);
  let selectedSite = $state<SiteSummary | null>(null);
  let mounted = false;
  let loadRequestId = 0;
  let selectionRequestId = 0;
  let refreshRunId = 0;

  let querySiteId = $derived(page.url.searchParams.get("siteId"));
  let selectedAssignment = $derived(assignments.find((assignment) => assignment.siteId === selectedSiteId));
  let selectedRouteQuery = $derived.by((): string => {
    return workflowQuery({
      inspectionId: selectedAssignment?.inspectionId ?? page.url.searchParams.get("inspectionId"),
      siteId: selectedSiteId,
    });
  });
  let evidenceRoute = $derived(`/evidence${selectedRouteQuery}` as EvidenceRoute);

  function preserveSelectedContext(siteId: string): void {
    const assignment = assignments.find((item) => item.siteId === siteId);
    const query = workflowQuery({ inspectionId: assignment?.inspectionId ?? null, siteId });
    const route = `/inspection${query}` as InspectionRoute;
    const href = resolve(route);
    void goto(href, { replaceState: true, noScroll: true, keepFocus: true });
  }

  function resetRefreshTrace(): void {
    refreshRunId += 1;
    refreshing = false;
  }

  function dispatchLiveUpdate(update: Omit<LocalOperationUpdate, "id" | "occurredAt">): void {
    if (typeof window === "undefined") return;
    const occurredAt = new Date().toISOString();
    const detail = {
      ...update,
      id: `${update.operationId}-${update.state}-${occurredAt}`,
      occurredAt,
    };
    window.dispatchEvent(new CustomEvent<LocalOperationUpdate>("trellisoperationupdate", {
      detail,
    }));
  }

  async function loadSite(siteId: string): Promise<void> {
    const requestId = ++selectionRequestId;
    if (selectedSiteId !== siteId) resetRefreshTrace();
    selectedSiteId = siteId;
    preserveSelectedContext(siteId);
    error = null;

    try {
      const response = await trellis.sitesGet({ siteId }).orThrow();
      if (!mounted || requestId !== selectionRequestId || selectedSiteId !== siteId) return;
      selectedSite = response.site ?? null;
    } catch (cause) {
      if (!mounted || requestId !== selectionRequestId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    }
  }

  async function loadDesk(preferredSiteId?: string): Promise<void> {
    const requestId = ++loadRequestId;
    selectionRequestId += 1;
    loadingAssignments = true;
    loadingSites = true;
    error = null;

    try {
      const [assignmentResponse, siteResponse] = await Promise.all([
        trellis.assignmentsList(listPage).orThrow(),
        trellis.sitesList(listPage).orThrow(),
      ]);
      if (!mounted || requestId !== loadRequestId) return;

      assignments = assignmentResponse.items;
      sites = siteResponse.items;
      const siteIdToLoad = preferredSiteId ?? selectedSiteId ?? assignmentResponse.items[0]?.siteId ?? siteResponse.items[0]?.siteId;

      if (siteIdToLoad) {
        if (selectedSiteId !== siteIdToLoad) resetRefreshTrace();
        selectedSiteId = siteIdToLoad;
        const detail = await trellis.sitesGet({ siteId: siteIdToLoad }).orThrow();
        if (!mounted || requestId !== loadRequestId || selectedSiteId !== siteIdToLoad) return;
        selectedSite = detail.site ?? null;
      } else {
        selectedSiteId = null;
        selectedSite = null;
      }
    } catch (cause) {
      if (!mounted || requestId !== loadRequestId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted || requestId !== loadRequestId) return;
      loadingAssignments = false;
      loadingSites = false;
    }
  }

  function isTerminalState(state: string): boolean {
    return state === "completed" || state === "failed" || state === "cancelled";
  }

  async function watchRefresh(ref: RefreshOperationRef, runId: number, onTerminal: (terminal: RefreshTerminal) => void): Promise<void> {
    const stream = await ref.watch().orThrow();
    for await (const event of stream) {
      if (!mounted || runId !== refreshRunId) {
        return;
      }
      if (isTerminalState(event.snapshot.state)) {
        onTerminal({ state: event.snapshot.state, output: event.snapshot.output });
        return;
      }
      if (!event.progress) continue;
      const label = `${event.progress.stage}: ${event.progress.message}`;
      dispatchLiveUpdate({
        kind: "operation",
        operationId: ref.id,
        name: "Sites.Refresh",
        action: label,
        subject: selectedSite?.siteName ?? selectedSiteId ?? "Selected site",
        state: event.snapshot.state,
      });
    }
  }

  async function refreshSite(): Promise<void> {
    if (!selectedSiteId) return;

    refreshing = true;
    error = null;
    const runId = ++refreshRunId;
    let terminalReached = false;
    let completionUpdateDispatched = false;
    let operationId = `Sites.Refresh-${runId}`;

    const applyTerminal = (terminal: RefreshTerminal): void => {
      terminalReached = true;
      refreshing = false;
      if (completionUpdateDispatched) return;
      completionUpdateDispatched = true;
      dispatchLiveUpdate({
        kind: "operation",
        operationId,
        name: "Sites.Refresh",
        action: terminal.state === "completed" ? "Completed field status refresh" : "Field status refresh ended",
        subject: terminal.output?.site.siteName ?? selectedSite?.siteName ?? selectedSiteId ?? "Selected site",
        state: terminal.state,
        refreshId: terminal.output?.refreshId,
      });
      if (terminal.output) {
        const refreshedSite = terminal.output.site;
        selectedSite = refreshedSite;
        sites = sites.map((site) => site.siteId === refreshedSite.siteId ? refreshedSite : site);
      }
    };

    try {
      const ref = await trellis.sitesRefresh({ siteId: selectedSiteId }).start().orThrow();
      if (!mounted || runId !== refreshRunId) return;
      operationId = ref.id;
      dispatchLiveUpdate({
        kind: "operation",
        operationId: ref.id,
        name: "Sites.Refresh",
        action: "Started field status refresh",
        subject: selectedSite?.siteName ?? selectedSiteId ?? "Selected site",
        state: "started",
      });
      void watchRefresh(ref, runId, applyTerminal).catch((cause) => {
        if (!mounted || runId !== refreshRunId) return;
        if (terminalReached) {
          error = `Sites.Refresh completed, but the progress watch failed: ${cause instanceof Error ? cause.message : String(cause)}`;
          return;
        }
        error = cause instanceof Error ? cause.message : String(cause);
        dispatchLiveUpdate({
          kind: "operation",
          operationId: ref.id,
          name: "Sites.Refresh",
          action: "Progress watch failed",
          subject: selectedSite?.siteName ?? selectedSiteId ?? "Selected site",
          state: "failed",
        });
      });
      const terminal = await ref.wait().orThrow();
      if (!mounted || runId !== refreshRunId) return;
      terminalReached = true;
      error = null;
      applyTerminal(terminal);
    } catch (cause) {
      if (!mounted || runId !== refreshRunId) return;
      if (terminalReached) {
        error = `Sites.Refresh completed, but the wait request failed: ${cause instanceof Error ? cause.message : String(cause)}`;
        return;
      }
      error = cause instanceof Error ? cause.message : String(cause);
      dispatchLiveUpdate({
        kind: "operation",
        operationId,
        name: "Sites.Refresh",
        action: "Failed field status refresh",
        subject: selectedSite?.siteName ?? selectedSiteId ?? "Selected site",
        state: "failed",
      });
    } finally {
      if (!mounted || runId !== refreshRunId) return;
      refreshing = false;
    }
  }

  function priorityClass(priority: InspectionAssignment["priority"]): string {
    if (priority === "high") return "badge badge-error badge-outline";
    if (priority === "medium") return "badge badge-warning badge-outline";
    return "badge badge-success badge-outline";
  }

  function selectAssignmentWithKeyboard(event: KeyboardEvent, siteId: string): void {
    if (event.key !== "Enter" && event.key !== " ") return;
    event.preventDefault();
    void loadSite(siteId);
  }

  onMount(() => {
    mounted = true;
    void loadDesk(querySiteId ?? undefined);
  });

  onDestroy(() => {
    mounted = false;
    loadRequestId += 1;
    selectionRequestId += 1;
    refreshRunId += 1;
  });
</script>

<svelte:head>
  <title>Inspection Desk · Field Inspection Desk</title>
</svelte:head>

<section class="page-sheet rounded-box p-5 sm:p-7">
  <div class="flex flex-col gap-6">
    <header class="pb-1">
      <div class="flex min-w-0 flex-col gap-5 lg:flex-row lg:items-end lg:justify-between">
        <div class="min-w-0 space-y-3">
          <div class="trellis-kicker">Workflow step 1</div>
          <h1 class="break-words text-2xl font-black tracking-tight md:text-3xl">Set the active inspection</h1>
          <p class="max-w-3xl break-words text-sm text-base-content/70">
            Choose the inspection, review live site context, then continue to evidence verification.
          </p>
        </div>
      </div>
      <p class="capability-note mt-4">
        <strong>RPC + operation:</strong> Assignments.List + Sites.List + Sites.Get + Sites.Refresh
      </p>
    </header>

    <div class="workflow-progress-strip grid gap-3 border-y border-base-300/80 py-4 text-sm md:grid-cols-4" aria-label="Inspection workflow steps">
      <div class="workflow-progress-item" aria-current="step">
        <span class="workflow-step-index" aria-hidden="true">1</span>
        <span class="min-w-0"><strong class="block">Inspection</strong><span class="text-base-content/64">Select and review</span></span>
      </div>
      <div class={[
        "workflow-progress-item",
        !selectedSite && "opacity-55",
      ]}>
        <span class="workflow-step-index" aria-hidden="true">2</span>
        <span class="min-w-0"><strong class="block">Evidence</strong><span class="text-base-content/64">Verify photos</span></span>
      </div>
      <div class={[
        "workflow-progress-item",
        !selectedSite && "opacity-55",
      ]}>
        <span class="workflow-step-index" aria-hidden="true">3</span>
        <span class="min-w-0"><strong class="block">Closeout</strong><span class="text-base-content/64">Publish report</span></span>
      </div>
      <div class={[
        "workflow-progress-item",
        !selectedSite && "opacity-55",
      ]}>
        <span class="workflow-step-index" aria-hidden="true">4</span>
        <span class="min-w-0"><strong class="block">Final report</strong><span class="text-base-content/64">Closeout</span></span>
      </div>
    </div>

    <div class="stats stats-vertical overflow-hidden border-y border-base-300/80 bg-base-200/35 md:stats-horizontal">
      <div class="stat">
        <div class="stat-title">Queued inspections</div>
        <div class="stat-value text-3xl">{assignments.length}</div>
        <div class="stat-desc">Live Trellis response</div>
      </div>
      <div class="stat">
        <div class="stat-title min-w-0 break-words">Active status</div>
        <div class="stat-value min-w-0 break-words text-lg">{selectedSite?.latestStatus ?? "No site selected"}</div>
        <div class="stat-desc">Live Trellis response</div>
      </div>
    </div>

    {#if error}
      <div role="alert" class="alert alert-error"><span>{error}</span></div>
    {/if}

    <div class="section-rule grid gap-7 pt-6 xl:grid-cols-[minmax(0,1.15fr)_minmax(20rem,0.85fr)]">
      <section class="min-w-0">
        <div class="flex flex-col gap-5">
          <div class="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <div class="min-w-0">
              <p class="source-label">Step 1</p>
              <h2 class="mt-1 min-w-0 break-words text-lg font-black tracking-tight">Assignment queue</h2>
            </div>
            <button class="btn btn-square btn-outline btn-sm" onclick={() => void loadDesk()} disabled={loadingAssignments || loadingSites || refreshing} aria-label="Refresh assignment queue">
              {#if loadingAssignments || loadingSites}
                <span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
              {:else}
                <svg aria-hidden="true" class="h-4 w-4" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <path d="M21 12a9 9 0 0 1-15.5 6.2" />
                  <path d="M3 12A9 9 0 0 1 18.5 5.8" />
                  <path d="M18.5 2v3.8H22" />
                  <path d="M5.5 22v-3.8H2" />
                </svg>
              {/if}
            </button>
          </div>

          {#if loadingAssignments}
            <div class="alert" role="status"><span>Loading assignments and site summaries.</span></div>
          {:else if assignments.length === 0}
            <div class="alert"><span>No records returned from Assignments.List. Confirm the demo service is seeded, or refresh the desk.</span></div>
          {:else}
            <div class="overflow-x-auto">
              <table class="table table-zebra executive-table min-w-[34rem]">
                <thead>
                  <tr><th>Inspection stop</th><th>Asset</th><th>Priority</th></tr>
                </thead>
                <tbody>
                  {#each assignments as assignment (assignment.inspectionId)}
                    <tr
                      class={[
                        "cursor-pointer hover:bg-base-200/80 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent",
                        selectedSiteId === assignment.siteId && "assignment-row-selected",
                      ]}
                      role="button"
                      tabindex="0"
                      aria-label={`Select ${assignment.siteName}`}
                      aria-pressed={selectedSiteId === assignment.siteId}
                      onclick={() => void loadSite(assignment.siteId)}
                      onkeydown={(event) => selectAssignmentWithKeyboard(event, assignment.siteId)}
                    >
                      <th scope="row">
                        <div class="break-words font-medium">{assignment.siteName}</div>
                        <div class="break-words text-xs text-base-content/60">{assignment.checklistName}</div>
                      </th>
                      <td>
                        <div class="break-words">{assignment.assetName}</div>
                        <div class="break-words font-mono text-xs text-base-content/60">{assignment.inspectionId}</div>
                      </td>
                      <td><span class={priorityClass(assignment.priority)}>{assignment.priority}</span></td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        </div>
      </section>

      <section class="min-w-0 border-t border-base-300/80 pt-6 xl:border-l xl:border-t-0 xl:pl-6 xl:pt-0">
        <div class="flex flex-col gap-5">
          <div class="flex min-w-0 flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div class="min-w-0">
              <p class="source-label">Step 1 context</p>
              <h2 class="mt-1 min-w-0 break-words text-lg font-black tracking-tight">Site context</h2>
            </div>
            <button class="btn btn-outline btn-sm shrink-0" onclick={refreshSite} disabled={refreshing || !selectedSiteId}>
              {#if refreshing}
                <span class="loading loading-spinner loading-xs" aria-hidden="true"></span>
                Updating status
              {:else}
                Update field status
              {/if}
            </button>
          </div>

          {#if selectedSite}
            <dl class="divide-y divide-base-300/80 border-y border-base-300/80 text-sm">
              <div class="grid gap-1 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-4">
                <dt class="source-label">Site</dt>
                <dd class="min-w-0 break-words font-medium">{selectedSite.siteName}</dd>
              </div>
              <div class="grid gap-1 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-4">
                <dt class="source-label">Field status</dt>
                <dd class="min-w-0 break-words">{selectedSite.latestStatus}</dd>
              </div>
              <div class="grid gap-1 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-4">
                <dt class="source-label">Open inspections</dt>
                <dd>{selectedSite.openInspections}</dd>
              </div>
              <div class="grid gap-1 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-4">
                <dt class="source-label">Overdue inspections</dt>
                <dd>{selectedSite.overdueInspections}</dd>
              </div>
              <div class="grid gap-1 py-3 sm:grid-cols-[11rem_minmax(0,1fr)] sm:gap-4">
                <dt class="source-label">Last report</dt>
                <dd class="min-w-0 break-words text-xs leading-5">
                  <span>{formatDateTime(selectedSite.lastReportAt)}</span>
                  {#if formatRelativeAge(selectedSite.lastReportAt)}
                    <span class="block">({formatRelativeAge(selectedSite.lastReportAt)})</span>
                  {/if}
                </dd>
              </div>
            </dl>
          {:else}
            <div class="alert"><span>Select an inspection to load live context from Sites.Get.</span></div>
          {/if}

          {#if selectedSite}
            <div class="next-action-rail px-1 py-4">
              <p class="source-label">Primary continuation</p>
              <div class="mt-3 flex flex-wrap gap-3">
                <a class="btn btn-accent btn-sm" href={resolve(evidenceRoute)}>Next: verify evidence</a>
              </div>
            </div>
          {/if}
        </div>
      </section>
    </div>
  </div>
</section>
