<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { afterNavigate, beforeNavigate } from "$app/navigation";
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import { getTrellis } from "$lib/trellis";

  type InspectionAssignment = { inspectionId: string; siteId?: string; siteName: string; assetName: string };
  type ReportsGenerateProgress = { stage: string; message: string };
  type ReportsGenerateResponse = { reportId: string; inspectionId: string; status: string };
  type ReportEvent = { type: string; snapshot: { state: string }; progress?: ReportsGenerateProgress };
  type ReportTerminal = { state: "completed" | "failed" | "cancelled"; output?: ReportsGenerateResponse };
  type ReportOperationRef = {
    id: string;
    watch(): { orThrow(): Promise<AsyncIterable<ReportEvent>> };
    wait(): { orThrow(): Promise<ReportTerminal> };
  };
  type PublishState = "idle" | "running" | "completed" | "failed" | "cancelled";
  type ReportsRoute = "/(app)/reports" | `/(app)/reports?${string}`;
  type LocalOperationUpdate = {
    kind: "operation";
    id: string;
    operationId: string;
    name: "Reports.Generate";
    action: string;
    subject: string;
    state: string;
    occurredAt: string;
    inspectionId: string;
  };
  type ReportsGenerateInputWithComment = { inspectionId: string; reportComment: string };

  const trellis = getTrellis();

  let assignments = $state<InspectionAssignment[]>([]);
  let selectedInspectionId = $state("");
  let reportComment = $state("Final closeout report approved for executive review.");
  let loadingAssignments = $state(true);
  let running = $state(false);
  let error = $state<string | null>(null);
  let publishState = $state<PublishState>("idle");
  let inlineAction = $state("Ready to publish the final report.");
  let acceptedId = $state<string | null>(null);
  let terminal = $state<ReportTerminal | null>(null);
  let currentRef: ReportOperationRef | null = null;
  let mounted = false;
  let assignmentRequestId = 0;
  let operationRunId = 0;
  const listPage = { page: { limit: 50 } };

  let queryInspectionId = $derived(page.url.searchParams.get("inspectionId"));
  let selectedAssignment = $derived(assignments.find((assignment) => assignment.inspectionId === selectedInspectionId));
  let publishTarget = $derived(selectedAssignment ? `${selectedAssignment.siteName} · ${selectedAssignment.assetName}` : selectedInspectionId || "Selected inspection");
  let reportCommentValid = $derived(reportComment.trim().length > 0);
  let reportsRoute = $derived.by((): ReportsRoute => {
    if (!terminal?.output?.reportId) return "/(app)/reports";
    const params = new URLSearchParams({ reportId: terminal.output.reportId });
    return `/(app)/reports?${params.toString()}` as ReportsRoute;
  });

  function dispatchLiveUpdate(update: Omit<LocalOperationUpdate, "id" | "occurredAt">): void {
    if (typeof window === "undefined") return;
    const occurredAt = new Date().toISOString();
    window.dispatchEvent(new CustomEvent<LocalOperationUpdate>("trellisoperationupdate", {
      detail: {
        ...update,
        id: `${update.operationId}-${update.state}-${occurredAt}`,
        occurredAt,
      },
    }));
  }

  function updateInlineState(state: PublishState, action: string): void {
    publishState = state;
    inlineAction = action;
  }

  async function loadAssignments(preferredInspectionId?: string): Promise<void> {
    const requestId = ++assignmentRequestId;
    loadingAssignments = true;
    error = null;

    try {
      const response = await trellis.assignmentsList(listPage).orThrow();
      if (!mounted || requestId !== assignmentRequestId) return;
      assignments = response.items;
      selectedInspectionId = response.items.some((assignment) => assignment.inspectionId === preferredInspectionId)
        ? preferredInspectionId ?? ""
        : response.items[0]?.inspectionId ?? "";
    } catch (cause) {
      if (!mounted || requestId !== assignmentRequestId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted || requestId !== assignmentRequestId) return;
      loadingAssignments = false;
    }
  }

  function selectInspection(event: Event): void {
    const target = event.currentTarget;
    if (!(target instanceof HTMLSelectElement)) return;

    selectedInspectionId = target.value;
    terminal = null;
    acceptedId = null;
    updateInlineState("idle", "Ready to publish the final report.");
  }

  async function watchOperation(ref: ReportOperationRef, runId: number): Promise<void> {
    const stream = await ref.watch().orThrow();
    for await (const event of stream) {
      if (!mounted || runId !== operationRunId) return;
      const action = event.progress ? `${event.progress.stage}: ${event.progress.message}` : "Operation update received";
      updateInlineState("running", action);
      dispatchLiveUpdate({
        kind: "operation",
        operationId: ref.id,
        name: "Reports.Generate",
        action,
        subject: publishTarget,
        state: event.snapshot.state,
        inspectionId: selectedInspectionId,
      });
    }
  }

  async function startOperation(): Promise<void> {
    const comment = reportComment.trim();
    if (!selectedInspectionId || !comment || running) return;

    running = true;
    error = null;
    terminal = null;
    acceptedId = null;
    updateInlineState("running", "Starting final report publication.");
    const runId = ++operationRunId;

    try {
      const reportInput: ReportsGenerateInputWithComment = { inspectionId: selectedInspectionId, reportComment: comment };
      const ref = await trellis.reportsGenerate(reportInput).start().orThrow();
      if (!mounted || runId !== operationRunId) return;
      currentRef = ref;
      acceptedId = ref.id;
      dispatchLiveUpdate({
        kind: "operation",
        operationId: ref.id,
        name: "Reports.Generate",
        action: "Started final report publication",
        subject: publishTarget,
        state: "running",
        inspectionId: selectedInspectionId,
      });
      void watchOperation(ref, runId).catch((cause) => {
        if (!mounted || runId !== operationRunId) return;
        error = cause instanceof Error ? cause.message : String(cause);
        dispatchLiveUpdate({
          kind: "operation",
          operationId: ref.id,
          name: "Reports.Generate",
          action: "Progress watch failed",
          subject: publishTarget,
          state: "failed",
          inspectionId: selectedInspectionId,
        });
      });
      const completed = await ref.wait().orThrow();
      if (!mounted || runId !== operationRunId) return;
      terminal = completed;
      updateInlineState(completed.state, completed.state === "completed" ? "Final report published." : `Report publication ${completed.state}.`);
      dispatchLiveUpdate({
        kind: "operation",
        operationId: ref.id,
        name: "Reports.Generate",
        action: completed.state === "completed" ? "Completed final report publication" : `Report publication ${completed.state}`,
        subject: publishTarget,
        state: completed.state,
        inspectionId: selectedInspectionId,
      });
    } catch (cause) {
      if (!mounted || runId !== operationRunId) return;
      const message = cause instanceof Error ? cause.message : String(cause);
      error = message;
      updateInlineState("failed", "Report publication failed.");
      dispatchLiveUpdate({
        kind: "operation",
        operationId: acceptedId ?? `Reports.Generate-${runId}`,
        name: "Reports.Generate",
        action: "Failed final report publication",
        subject: publishTarget,
        state: "failed",
        inspectionId: selectedInspectionId,
      });
    } finally {
      if (!mounted || runId !== operationRunId) return;
      running = false;
      currentRef = null;
    }
  }

  function stateBadgeClass(state: PublishState): string {
    if (state === "completed") return "badge badge-success badge-outline";
    if (state === "cancelled") return "badge badge-warning badge-outline";
    if (state === "failed") return "badge badge-error badge-outline";
    return "badge badge-outline";
  }

  beforeNavigate((navigation) => {
    if (!running) return;
    navigation.cancel();
    error = "Closeout is running. Wait for Trellis to finish before leaving.";
  });

  afterNavigate(() => {
    if (!mounted || running) return;
    const preferredInspectionId = page.url.searchParams.get("inspectionId");
    if (preferredInspectionId && preferredInspectionId !== selectedInspectionId) {
      void loadAssignments(preferredInspectionId);
    }
  });

  onMount(() => {
    mounted = true;
    void loadAssignments(queryInspectionId ?? undefined);
  });

  onDestroy(() => {
    mounted = false;
    assignmentRequestId += 1;
    operationRunId += 1;
    currentRef = null;
  });
</script>

<svelte:head>
  <title>Closeout Desk · Field Inspection Desk</title>
</svelte:head>

<section class="page-sheet rounded-box p-5 sm:p-7">
  <div class="flex flex-col gap-8">
    <header class="pb-1">
      <div class="flex min-w-0 flex-col gap-5 lg:flex-row lg:items-start lg:justify-between">
        <div class="min-w-0 space-y-3">
          <div class="trellis-kicker">Closeout desk</div>
          <h1 class="break-words text-2xl font-black tracking-tight md:text-3xl">Publish the final inspection report</h1>
          <p class="max-w-3xl break-words text-sm text-base-content/70">
            Select the inspection, add the required report comment, and start Reports.Generate as the final action in the workflow.
          </p>
          {#if selectedInspectionId}
            <p class="source-label">Finalizing {selectedInspectionId}</p>
          {/if}
        </div>
      </div>
      <p class="capability-note mt-4">
        <strong>RPC + operation:</strong> Assignments.List + Reports.Generate start, watch, wait
      </p>
    </header>

    <div class="workflow-progress-strip grid gap-3 border-y border-base-300/80 py-4 text-sm md:grid-cols-4" aria-label="Inspection workflow steps">
      <div class="workflow-progress-item">
        <span class="workflow-step-index" aria-hidden="true">1</span>
        <span class="min-w-0">
          <strong class="block">Inspection</strong>
          <span class="text-base-content/64">Context selected</span>
        </span>
      </div>
      <div class="workflow-progress-item">
        <span class="workflow-step-index" aria-hidden="true">2</span>
        <span class="min-w-0">
          <strong class="block">Evidence</strong>
          <span class="text-base-content/64">Photos verified</span>
        </span>
      </div>
      <div class="workflow-progress-item">
        <span class="workflow-step-index" aria-hidden="true">3</span>
        <span class="min-w-0">
          <strong class="block">Readiness</strong>
          <span class="text-base-content/64">Queue approved</span>
        </span>
      </div>
      <div class="workflow-progress-item" aria-current="step">
        <span class="workflow-step-index" aria-hidden="true">4</span>
        <span class="min-w-0">
          <strong class="block">Closeout</strong>
          <span class="text-base-content/64">Publish final report</span>
        </span>
      </div>
    </div>

    {#if error}
      <div role="alert" class="alert alert-error"><span>{error}</span></div>
    {/if}

    <section class="section-rule pt-8" aria-labelledby="publish-report-heading" aria-busy={running}>
      <div class="grid gap-8 xl:grid-cols-[minmax(0,1fr)_minmax(18rem,0.65fr)]">
        <div class="min-w-0 space-y-6">
          <div class="flex min-w-0 items-start gap-3">
            <span class="flex size-8 shrink-0 items-center justify-center rounded-full bg-accent text-sm font-black text-accent-content">4</span>
            <div class="min-w-0">
              <h2 id="publish-report-heading" class="break-words text-lg font-black tracking-tight">Final report publication</h2>
              <p class="mt-1 break-words text-sm text-base-content/70">This starts <code class="font-mono text-xs">Reports.Generate</code> with the selected inspection and report comment.</p>
            </div>
          </div>

          {#if loadingAssignments}
            <div class="alert" role="status"><span>Loading inspection candidates from Assignments.List.</span></div>
          {:else if assignments.length === 0}
            <div class="alert"><span>No inspection candidates returned from Assignments.List. Refresh after the demo service is seeded.</span></div>
          {:else}
            <div class="grid gap-5">
              <label class="form-control gap-2">
                <span class="label-text font-medium">Inspection to close out</span>
                <select class="select select-bordered w-full min-w-0" value={selectedInspectionId} onchange={selectInspection} disabled={running}>
                  {#each assignments as assignment (assignment.inspectionId)}
                    <option value={assignment.inspectionId}>{assignment.siteName} · {assignment.assetName}</option>
                  {/each}
                </select>
              </label>

              <label class="form-control gap-2">
                <span class="label-text font-medium">Report comment</span>
                <textarea class="textarea textarea-bordered min-h-32 w-full" bind:value={reportComment} disabled={running} required aria-describedby="report-comment-help"></textarea>
                <span id="report-comment-help" class="text-xs text-base-content/58">Required. This comment is written into the published report.</span>
              </label>

              <div class="flex flex-wrap gap-3">
                <button class="btn btn-accent" onclick={startOperation} disabled={running || !selectedInspectionId || !reportCommentValid}>
                  {running ? "Publishing final report..." : "Publish final report"}
                </button>
                {#if terminal?.output}
                  <a class="btn btn-outline" href={resolve(reportsRoute)}>View report</a>
                {/if}
              </div>
            </div>
          {/if}
        </div>

        <aside class="border-y border-base-300/80 bg-base-200/35 px-1 py-4" aria-live="polite">
          <div class="flex min-w-0 flex-wrap items-center justify-between gap-3">
            <div class="min-w-0">
              <p class="source-label">Reports.Generate</p>
              <p class="mt-2 break-words font-semibold">{publishTarget}</p>
            </div>
            <span class={stateBadgeClass(publishState)}>{publishState}</span>
          </div>

          <div class="mt-4 flex min-w-0 items-center gap-3 text-sm text-base-content/70">
            {#if running}
              <span class="loading loading-spinner loading-sm text-accent" aria-hidden="true"></span>
            {/if}
            <span class="min-w-0 break-words">{inlineAction}</span>
          </div>

          {#if acceptedId}
            <dl class="mt-4 grid gap-2 border-t border-base-300/80 pt-4 text-xs sm:grid-cols-[6rem_minmax(0,1fr)]">
              <dt class="text-base-content/58">Operation</dt><dd class="break-words font-mono">{acceptedId}</dd>
              <dt class="text-base-content/58">Inspection</dt><dd class="break-words font-mono">{selectedInspectionId}</dd>
            </dl>
          {/if}

          {#if terminal?.output}
            <p class="mt-4 border-t border-base-300/80 pt-4 text-sm text-base-content/70">
              Final report {terminal.output.reportId} is published and available in Reports.
            </p>
          {/if}
        </aside>
      </div>
    </section>
  </div>
</section>
