<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { LiveSubscription, RefreshScheduler } from "$lib/console/live_refresh.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { type apis } from "trellis-web-generated";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import JobsScopedCharts from "$lib/components/JobsScopedCharts.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import MetricsLedger from "$lib/components/MetricsLedger.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import Term from "$lib/components/Term.svelte";
  import { boundedNumber, compactDuration, errorMessage } from "$lib/format";
  import { loadJobsMetrics } from "$lib/jobs_metrics.ts";
  import {
    cancelJob,
    loadJobsQueryPage,
    loadJobsServices,
    loadJobsSummary,
  } from "$lib/jobs_page.ts";
  import { bulkExpectedCount, bulkTargetDetails, runBulk, toggleAll, toggleId } from "$lib/bulk.ts";
  import { getTrellis } from "$lib/trellis";
  import { nextCursorPage, previousCursorPage, resetCursorHistory } from "$lib/cursor_history.ts";
  import { SvelteSet } from "svelte/reactivity";

  type Job = apis.jobs.QueryOutput["items"][number];
  type JobState = Job["state"];
  type ServiceInfo = apis.jobs.ListServicesOutput["items"][number];
  type MetricsWindow = apis.jobs.MetricsInput["window"];
  type Focus = "running-risk" | "running" | "action" | "completed" | "failed" | "dead" | "backlog";
  type JobPathname = `/admin/jobs/${string}` & {};

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const rpcTimeout = 10_000;
  const windows: Array<{ value: MetricsWindow; label: string; title: string }> = [
    { value: "15m", label: "15m", title: "Last 15 minutes" },
    { value: "1h", label: "1h", title: "Last hour" },
    { value: "6h", label: "6h", title: "Last 6 hours" },
    { value: "24h", label: "24h", title: "Last 24 hours" },
    { value: "7d", label: "7d", title: "Last 7 days" },
  ];

  let loading = $state(true);
  let metricsLoading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let metricsError = $state<string | null>(null);
  let summaryError = $state<string | null>(null);
  let servicesError = $state<string | null>(null);
  let unavailableMessage = $state<string | null>(null);
  let services = $state.raw<ServiceInfo[]>([]);
  let jobs = $state.raw<Job[]>([]);
  let groups = $state.raw<apis.jobs.SummaryOutput["groups"]>([]);
  let stats = $state.raw<apis.jobs.SummaryOutput["stats"]>({ byState: {}, total: 0n });
  let jobCount = $state<bigint | null>(null);
  let cursor = $state<string | undefined>();
  let cursorBackStack = $state<string[]>([]);
  let nextCursor = $state<string | undefined>();
  let metrics = $state.raw<apis.jobs.MetricsOutput | null>(null);
  let metricsWindow = $state<MetricsWindow>("1h");
  let selectedJobType = $state<string | null>(null);
  let focus = $state<Focus>(asFocus(page.url.searchParams.get("focus")) ?? "running-risk");
  let handledFocusParam = page.url.searchParams.get("focus");

  afterNavigate(() => {
    const value = page.url.searchParams.get("focus");
    if (value === handledFocusParam) return;
    handledFocusParam = value;
    const next = value === "running-risk" || value === "running" || value === "action" || value === "completed" || value === "failed" || value === "dead" || value === "backlog" ? value : null;
    if (next && next !== focus) {
      focus = next;
      ({ cursor, back: cursorBackStack } = resetCursorHistory());
      nextCursor = undefined;
      selectedJobs.clear();
      void loadJobs(false);
    }
  });
  let lastUpdated = $state<Date | null>(null);
  let watchStatus = $state("Unavailable");
  let jobsSequence = 0;
  let metricsSequence = 0;
  let disposed = false;
  let watchController: AbortController | null = null;
  let subscription: LiveSubscription | null = null;
  const refreshScheduler = new RefreshScheduler({
    canRefresh: () => !disposed && document.visibilityState === "visible",
    onRefresh: () => refresh(false),
    onRefreshError: (cause) => { error = errorMessage(cause); },
  });
  const selectedJobs = new SvelteSet<string>();
  let bulkBusy = $state(false);
  let bulkResult = $state<{ succeeded: number; failed: string[] } | null>(null);
  let bulkUnknown = $state<string[]>([]);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const workerCount = $derived(services.reduce((sum, service) => sum + service.workers.length, 0));
  const windowLabel = $derived(windows.find((option) => option.value === metricsWindow)?.title ?? metricsWindow);
  const focusedJobs = $derived.by(() =>
    focus === "running-risk"
      ? [...jobs].sort((left, right) => riskPriority(left) - riskPriority(right) || boundedNumber((right.runtimeMs ?? 0n) - (left.runtimeMs ?? 0n)))
      : jobs,
  );
  const cancellableJobs = $derived(focusedJobs.filter((job) => job.state === "active" || job.state === "pending" || job.state === "retry"));
  const selectableJobIds = $derived(cancellableJobs.map((job) => job.id));
  const overview = $derived.by(() => {
    const failed = boundedNumber(stats.failed ?? stats.byState.failed ?? 0n);
    const dead = boundedNumber(stats.dead ?? stats.byState.dead ?? 0n);
    const retrying = boundedNumber(stats.byState.retry ?? 0n);
    const stale = boundedNumber(stats.byState.stale ?? 0n);
    const processed = boundedNumber(stats.byState.completed ?? 0n);
    const total = boundedNumber(stats.total);
    return {
      action: failed + dead + retrying + stale,
      backlog: boundedNumber(stats.queued ?? 0n),
      dead,
      failed,
      failureRate: total > 0 ? ((failed + dead) / total) * 100 : 0,
      processed,
      retrying,
      running: boundedNumber(stats.running ?? 0n),
      slow: boundedNumber(stats.slow ?? 0n),
      stale,
    };
  });

  function asFocus(value: string | null): Focus | null {
    if (value === "running-risk" || value === "running" || value === "action" || value === "completed" || value === "failed" || value === "dead" || value === "backlog") return value;
    return null;
  }

  function resolveMetricsStep(window: MetricsWindow): apis.jobs.MetricsInput["step"] {
    if (window === "15m" || window === "1h") return "1m";
    if (window === "6h") return "5m";
    if (window === "24h") return "15m";
    return "1h";
  }

  async function cancelJobs(targets: Job[], query: apis.jobs.QueryInput) {
    bulkBusy = true;
    bulkResult = null;
    const outcome = await runBulk(targets, async (job) => {
      if (disposed || JSON.stringify(buildQuery()) !== JSON.stringify(query) ||

        !cancellableJobs.some((current) => current.id === job.id && current.state === job.state)) {
        throw new Error("Target or authority changed before dispatch; no cancellation sent.");
      }
      await cancelJob({ action: (input) => trellis.cancel(input) }, job.id);
    }, (cause) => classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed");
    for (const job of targets) selectedJobs.delete(job.id);
    bulkResult = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((failure) => `${failure.target.id}: ${failure.reason}`),
    };
    bulkUnknown = [...new Set([...bulkUnknown, ...outcome.unknown.map((item) => item.target.id)])];
    bulkBusy = false;
    void loadJobs();
  }

  async function requestBulkCancel() {
    if (false) return;
    const query = buildQuery();
    const targets = cancellableJobs.filter((job) => selectedJobs.has(job.id));
    if (targets.length === 0) return;
    const confirmed = await confirmationModal?.confirm({
      title: `Cancel ${targets.length} job${targets.length === 1 ? "" : "s"}?`,
      message: "Queued and running work stops. Work already committed by a job is not rolled back.",
      confirmLabel: `Cancel ${targets.length}`,
      targetLabel: "Jobs",
      targetName: `${targets.length} jobs`,
      expectedValue: bulkExpectedCount(targets.length),
      details: bulkTargetDetails(targets.map((job) => `${job.type} ${job.id}`)),
    });
    if (!confirmed || disposed || targets.some((target) => !cancellableJobs.some((current) => current.id === target.id && current.state === target.state)) ||

      JSON.stringify(buildQuery()) !== JSON.stringify(query)) return;
    await cancelJobs(targets, query);
  }

  function focusStates(value: Focus): JobState[] {
    if (value === "running-risk") return ["active", "retry"];
    if (value === "running") return ["active"];
    if (value === "action") return ["retry", "failed", "dead", "stale"];
    if (value === "completed") return ["completed"];
    if (value === "failed") return ["failed"];
    if (value === "dead") return ["dead"];
    return ["pending", "retry"];
  }

  function focusTitle(): string {
    const prefix = selectedJobType ? `${selectedJobType} · ` : "";
    if (focus === "running-risk") return `${prefix}Running and at risk`;
    if (focus === "running") return `${prefix}Running jobs`;
    if (focus === "action") return `${prefix}Jobs needing action`;
    if (focus === "completed") return `${prefix}Completed jobs`;
    if (focus === "failed") return `${prefix}Failed jobs`;
    if (focus === "dead") return `${prefix}Dead-lettered jobs`;
    return `${prefix}Backlog`;
  }

  function focusDescription(): string {
    if (focus === "running-risk") return "Waiting, slow, and retrying work first";
    if (focus === "running") return "Current active execution";
    if (focus === "action") return "Retrying, failed, dead, and stale work";
    if (focus === "completed") return "Most recently completed retained work";
    if (focus === "failed") return "Non-retryable failures available for inspection";
    if (focus === "dead") return "Exhausted deliveries awaiting replay or dismissal";
    return "Pending and retrying work, oldest first";
  }

  function buildQuery(): apis.jobs.QueryInput {
    return {
      page: { cursor, limit: 40 },
      groupBy: "type",
      state: focusStates(focus),
      type: selectedJobType ?? undefined,
      sort: {
        direction: "desc",
        field: focus === "backlog" ? "queueAge" : focus === "running" || focus === "running-risk" ? "runtime" : "updatedAt",
      },
    };
  }

  async function loadJobs(showLoading = true) {
    const sequence = ++jobsSequence;
    if (showLoading) loading = true;
    error = null;
    unavailableMessage = null;
    const query = buildQuery();
    // The job query is the primary result; service discovery and summary are
    // independent panels whose failure must not discard it.
    const [queryResult, servicesResult, summaryResult] = await Promise.allSettled([
      loadJobsQueryPage(
        { queryJobs: (input) => trellis.jobsQuery(input, { timeout: rpcTimeout }) },
        query,
      ),
      loadJobsServices({
        listServices: (input) => trellis.listServices(input, { timeout: rpcTimeout }),
      }),
      loadJobsSummary(
        { summarizeJobs: (input) => trellis.jobsSummary(input, { timeout: rpcTimeout }) },
        summaryScope(query),
      ),
    ]);
    if (sequence !== jobsSequence || disposed) return;
    try {
      if (queryResult.status === "fulfilled") {
        if (queryResult.value.available) {
          jobs = queryResult.value.jobs;
          nextCursor = queryResult.value.nextCursor;
        } else {
          unavailableMessage = queryResult.value.message;
          jobs = [];
          nextCursor = undefined;
        }
      } else {
        throw queryResult.reason;
      }
      if (servicesResult.status === "fulfilled" && servicesResult.value.available) {
        services = servicesResult.value.services;
        servicesError = null;
      } else {
        services = [];
        servicesError = servicesResult.status === "rejected"
          ? errorMessage(servicesResult.reason)
          : (servicesResult.value as { message: string }).message;
      }
      if (summaryResult.status === "fulfilled" && summaryResult.value.available) {
        groups = summaryResult.value.groups;
        stats = summaryResult.value.stats;
        jobCount = summaryResult.value.count;
        summaryError = null;
      } else {
        groups = [];
        stats = { byState: {}, total: 0n };
        jobCount = null;
        summaryError = summaryResult.status === "rejected"
          ? errorMessage(summaryResult.reason)
          : (summaryResult.value as { message: string }).message;
      }
    } catch (cause) {
      error = errorMessage(cause);
      jobs = [];
      nextCursor = undefined;
    } finally {
      loading = false;
    }
  }

  /** Common summary scope: supported filters without the focused state tab. */
  function summaryScope(query: apis.jobs.QueryInput): apis.jobs.QueryInput {
    const { page: _page, state: _state, sort: _sort, ...scope } = query;
    return scope;
  }

  async function loadMetrics() {
    const sequence = ++metricsSequence;
    metricsLoading = true;
    metricsError = null;
    try {
      const payload = await loadJobsMetrics(
        { metrics: (input) => trellis.jobsMetrics(input, { timeout: rpcTimeout }) },
        { groupBy: "type", step: resolveMetricsStep(metricsWindow), window: metricsWindow },
      );
      if (sequence !== metricsSequence || disposed) return;
      if (!payload.available) {
        metrics = null;
        metricsError = payload.message ?? "Jobs metrics are unavailable.";
        return;
      }
      metrics = payload.metrics ?? null;
    } catch (cause) {
      if (sequence !== metricsSequence || disposed) return;
      metrics = null;
      metricsError = errorMessage(cause);
    } finally {
      if (sequence === metricsSequence && !disposed) metricsLoading = false;
    }
  }

  async function refresh(showLoading = false) {
    if (!showLoading) refreshing = true;
    await Promise.all([loadJobs(showLoading), loadMetrics()]);
    lastUpdated = new Date();
    refreshing = false;
  }

  function selectFocus(value: Focus) {
    focus = value;
    resetPages();
    void loadJobs(false);
  }

  function selectJobType(value: string | null) {
    selectedJobType = value;
    resetPages();
    void loadJobs(false);
  }

  function resetPages() {
    ({ cursor, back: cursorBackStack } = resetCursorHistory());
    nextCursor = undefined;
    selectedJobs.clear();
  }

  function goPrevious() {
    const previous = { cursor, back: cursorBackStack };
    ({ cursor, back: cursorBackStack } = previousCursorPage(previous));
    void loadJobs().then(() => {
      // A failed page restores the previous cursor and keeps its data visible.
      if (error !== null || unavailableMessage !== null) {
        cursor = previous.cursor;
        cursorBackStack = previous.back;
      }
    });
  }

  function goNext() {
    if (!nextCursor) return;
    const previous = { cursor, back: cursorBackStack };
    ({ cursor, back: cursorBackStack } = nextCursorPage(previous, nextCursor));
    void loadJobs().then(() => {
      if (error !== null || unavailableMessage !== null) {
        cursor = previous.cursor;
        cursorBackStack = previous.back;
      }
    });
  }

  function selectWindow(value: MetricsWindow) {
    metricsWindow = value;
    void loadMetrics();
  }

  function jobRoute(id: string): JobPathname {
    return `/admin/jobs/${encodeURIComponent(id)}` as JobPathname;
  }

  function jobStateLabel(job: Job): string {
    if (job.state === "active" && (job.waitingOn?.length ?? 0) > 0) return "waiting";
    if (job.state === "active" && job.runtimeBand === "slow") return "slow";
    return job.state;
  }

  function riskPriority(job: Job): number {
    if ((job.waitingOn?.length ?? 0) > 0) return 0;
    if (job.state === "retry") return 1;
    if (job.runtimeBand === "slow") return 2;
    return 3;
  }

  function jobNarrative(job: Job): string {
    const wait = job.waitingOn?.[0];
    if (wait) return wait.label ?? wait.target.label ?? `Waiting on ${wait.target.kind}`;
    return job.progress?.message ?? job.progress?.step ?? job.lastError ?? "Execution in progress";
  }

  function jobDuration(job: Job): string {
    const value = job.state === "pending" || job.state === "retry" ? job.queueAgeMs : job.runtimeMs;
    return compactDuration(boundedNumber(value ?? 0n));
  }

  function jobProgress(job: Job): number {
    const current = job.progress?.current;
    const total = job.progress?.total;
    if (current === undefined || total === undefined || total <= 0n) return 0;
    return Math.min(100, Math.max(0, boundedNumber(current) / boundedNumber(total) * 100));
  }

  function jobStateVariant(job: Job): "healthy" | "degraded" | "unhealthy" | "offline" {
    const label = jobStateLabel(job);
    if (label === "completed") return "healthy";
    if (label === "waiting" || label === "slow" || label === "pending" || label === "retry") return "degraded";
    if (label === "failed" || label === "dead" || label === "stale") return "unhealthy";
    return "offline";
  }

  const ledgerItems = $derived([
    { id: "action", label: "Action needed", value: overview.action, detail: `${overview.failed} failed · ${overview.dead} dead · ${overview.retrying} retrying`, tone: "error" as const, active: focus === "action", attention: true },
    { id: "running", label: "Running", value: overview.running, detail: `${overview.slow} slow · ${workerCount} workers`, tone: "success" as const, active: focus === "running" || focus === "running-risk" },
    { id: "completed", label: "Processed", value: overview.processed.toLocaleString(), detail: "completed retained jobs", tone: "success" as const, active: focus === "completed" },
    { id: "failed", label: "Failed", value: overview.failed, detail: `${overview.failureRate.toFixed(2)}% matching`, tone: "error" as const, active: focus === "failed" },
    { id: "dead", label: "Dead", value: overview.dead, detail: "requires replay or dismissal", tone: "error" as const, active: focus === "dead" },
    { id: "backlog", label: "Backlog", value: overview.backlog, detail: "pending + retrying", tone: "warning" as const, active: focus === "backlog" },
  ]);

  function handleLedgerSelect(id: string) {
    if (id === "running") selectFocus("running");
    else if (id === "action" || id === "completed" || id === "failed" || id === "dead" || id === "backlog") selectFocus(id);
  }

  function stopWatch() {
    watchController?.abort();
    watchController = null;
    void subscription?.dispose();
    subscription = null;
    watchStatus = "Unavailable";
  }

  function startWatch() {
    stopWatch();
    const live = new LiveSubscription({
      subscribe: async () => {
        const controller = new AbortController();
        watchController = controller;
        const stream = await trellis.jobsWatch({ includeInitial: false }, { signal: controller.signal }).orThrow();
        void (async () => {
          try {
            for await (const _event of stream) {
              if (controller.signal.aborted || disposed) return;
              refreshScheduler.notify();
            }
            if (!controller.signal.aborted) live.closed();
          } catch (cause) {
            if (!controller.signal.aborted) live.closed(cause);
          }
        })();
      },
      unsubscribe: () => watchController?.abort(),
      onStatus: (status) => {
        if (!disposed) watchStatus = status === "live" ? "Live" : status === "reconnecting" ? "Reconnecting" : status === "connecting" ? "Connecting" : "Offline";
      },
    });
    subscription = live;
    void live.start();
  }

  $effect(() => {
untrack(() => {
      void refresh(true);
      startWatch();
    });
    return () => {
      ++jobsSequence;
      ++metricsSequence;
      stopWatch();
    };
  });

  onDestroy(() => {
    disposed = true;
    ++jobsSequence;
    ++metricsSequence;
    stopWatch();
    refreshScheduler.dispose();
  });
</script>

<svelte:head><title>Jobs · Trellis Console</title></svelte:head>

<section class="jobs-page">
  <PageToolbar title="Jobs" description="Execution health across services and job types.">
    {#snippet actions()}
      <div class="trellis-segment" role="group" aria-label="Metrics window">
        {#each windows as option (option.value)}
          <button
            type="button"
            class:active={metricsWindow === option.value}
            aria-pressed={metricsWindow === option.value}
            title={option.title}
            onclick={() => selectWindow(option.value)}
          >{option.label}</button>
        {/each}
      </div>
      <button class="btn btn-outline btn-sm" onclick={() => void refresh()} disabled={loading || refreshing}>
        {refreshing ? "Refreshing" : "Refresh"}
      </button>
    {/snippet}
  </PageToolbar>

  {#if lastUpdated}
    <p class="jobs-updated">Updated {lastUpdated.toLocaleTimeString()} · Live updates: <span aria-live="polite">{watchStatus}</span></p>
  {/if}

  {#if error}
    <Notice variant="error" role="alert">Jobs could not be loaded. {error}</Notice>
  {:else if unavailableMessage}
    <Notice variant="info" role="status">{unavailableMessage} Job processing can continue while visibility is unavailable.</Notice>
  {/if}

  {#if servicesError}
    <Notice variant="warning" role="status">
      Service discovery is unavailable: {servicesError} Job results are unaffected.
    </Notice>
  {/if}

  {#if summaryError}
    <Notice variant="warning" role="status">
      The job summary is unavailable: {summaryError} Job results are unaffected.
    </Notice>
  {/if}

  {#if bulkUnknown.length > 0}
    <Notice variant="warning" role="status">
      Cancellation outcome unknown for {bulkUnknown.join(", ")}. Inspect each job before any further action.
      <button class="btn btn-ghost btn-xs" type="button" onclick={() => bulkUnknown = []}>Dismiss after inspection</button>
    </Notice>
  {/if}

  {#if metricsError}
    <Notice variant="info" role="status">{metricsError}</Notice>
  {/if}

  {#if !loading && !unavailableMessage}
    {#if !summaryError}
      <MetricsLedger ariaLabel="Jobs status summary" items={ledgerItems} onSelect={handleLedgerSelect} />
    {/if}

    <div class="jobs-overview">
      {#if !summaryError}
      <Panel eyebrow="Secondary" title="Job-type health">
        {#snippet actions()}<span class="text-sm text-base-content/70">{groups.length} matching types</span>{/snippet}
        <p class="text-sm text-base-content/70">Complete retained totals by execution contract. Select a type to scope live work.</p>
        <DataTable>
          <thead><tr><th>Job type</th><th>Matching</th><th>Backlog</th><th>Failure rate</th><th>Oldest</th></tr></thead>
          <tbody>
            {#each groups as group (group.key)}
              <tr class:row-selected={selectedJobType === group.key}>
                <td><button type="button" class="link link-hover trellis-identifier" onclick={() => selectJobType(selectedJobType === group.key ? null : group.key)}>{group.label}</button></td>
                <td class="tabular-nums">{group.count.toLocaleString()}</td>
                <td class="tabular-nums">{(group.depth ?? 0n).toLocaleString()}</td>
                <td class="tabular-nums">{group.failureRate === undefined ? "—" : `${(group.failureRate * 100).toFixed(1)}%`}</td>
                <td>{group.oldestCreatedAt ? compactDuration(Date.now() - new Date(group.oldestCreatedAt).getTime()) : "—"}</td>
              </tr>
            {:else}
              <tr><td colspan="5">No job types match this operational view.</td></tr>
            {/each}
          </tbody>
        </DataTable>
      </Panel>
      {/if}
      {#if metricsLoading && !metrics}
        <LoadingState label="Loading job trends" />
      {:else if metrics}
        <JobsScopedCharts buckets={metrics.buckets} selectedKey={selectedJobType} {windowLabel} />
      {/if}
    </div>
  {/if}

  {#if !unavailableMessage}
    <Panel eyebrow="Primary" title={focusTitle()}>
      {#snippet actions()}
        <div class="flex items-center gap-3">
          <span class="text-sm text-base-content/70">{jobCount === null ? "Count unavailable" : `${jobCount} in scope (all states)`}</span>
          {#if selectedJobType}
            <button type="button" class="btn btn-ghost btn-sm" onclick={() => selectJobType(null)}>Clear type</button>
          {/if}
        </div>
      {/snippet}
      <p class="text-sm text-base-content/70">{focusDescription()}</p>
      {#if focus === "dead"}
        <p class="text-sm text-base-content/70">The <Term term="DLQ" /> holds jobs awaiting a replay or dismissal decision.</p>
      {/if}
      {#if bulkResult}
        <BulkResult
          succeeded={bulkResult.succeeded}
          failed={bulkResult.failed}
          pastTense="jobs cancelled"
          onDismiss={() => { bulkResult = null; }}
        />
      {:else if selectedJobs.size > 0}
        <BulkActionBar count={selectedJobs.size} noun="job" onClear={() => selectedJobs.clear()}>
          {#snippet actions()}
            <button class="btn btn-error btn-outline btn-sm" disabled={bulkBusy} onclick={() => void requestBulkCancel()}>
              {bulkBusy ? "Cancelling…" : "Cancel selected"}
            </button>
          {/snippet}
        </BulkActionBar>
      {/if}
      {#if loading}
        <LoadingState label="Loading focused jobs" class="min-h-32" />
      {:else if jobs.length === 0}
        <EmptyState title="No matching jobs" description="No retained jobs match this operational view." />
      {:else}
        <DataTable>
          <thead><tr>
            <th>
              <span class="sr-only">Select all cancellable jobs</span>
              <input
                type="checkbox"
                class="checkbox checkbox-xs"
                aria-label="Select all cancellable jobs"
                disabled={bulkBusy || selectableJobIds.length === 0}
                checked={selectableJobIds.length > 0 && selectableJobIds.every((id) => selectedJobs.has(id))}
                indeterminate={selectableJobIds.some((id) => selectedJobs.has(id)) && !selectableJobIds.every((id) => selectedJobs.has(id))}
                onchange={() => toggleAll(selectedJobs, selectableJobIds)}
              />
            </th>
            <th>Job type / id</th><th>Queue key</th><th>State</th><th>Status</th><th>Duration</th><th>Attempt</th></tr></thead>
          <tbody>
            {#each focusedJobs as job (job.id)}
              {@const progress = jobProgress(job)}
              <tr>
                <td>
                  {#if job.state === "active" || job.state === "pending" || job.state === "retry"}
                    <input
                      type="checkbox"
                      class="checkbox checkbox-xs"
                      aria-label={`Select job {job.id}`}
                      disabled={bulkBusy}
                      checked={selectedJobs.has(job.id)}
                      onchange={() => toggleId(selectedJobs, job.id)}
                    />
                  {:else}
                    <span class="text-xs text-base-content/50">—</span>
                  {/if}
                </td>
                <td class="min-w-0">
                  <a class="link link-hover block max-w-md truncate text-left trellis-identifier font-medium" href={resolve(jobRoute(job.id))}>{job.type}</a>
                  <span class="trellis-metadata trellis-identifier block max-w-md truncate">{job.id} · {job.service}</span>
                </td>
                <td class="trellis-metadata trellis-identifier block max-w-xs truncate">{job.queueKey ?? "Unkeyed"}</td>
                <td><StatusBadge label={jobStateLabel(job)} status={jobStateVariant(job)} /></td>
                <td class="max-w-md">
                  <span class="block truncate text-sm" title={jobNarrative(job)}>{jobNarrative(job)}</span>
                  {#if job.progress?.step}<span class="trellis-metadata block truncate">{job.progress.step}</span>{/if}
                  {#if progress > 0}<span class="jobs-progress"><i style:--job-progress={`${progress}%`}></i></span>{/if}
                </td>
                <td class="whitespace-nowrap tabular-nums text-sm">{job.state === "pending" || job.state === "retry" ? "Queue " : "Run "}{jobDuration(job)}</td>
                <td class="tabular-nums text-sm">{job.tries}/{job.maxTries}</td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
        {#if cursorBackStack.length > 0 || nextCursor}
          <nav class="flex items-center justify-end gap-3 text-sm text-base-content/70" aria-label="Jobs pages">
            <button class="btn btn-outline btn-xs" onclick={goPrevious} disabled={cursorBackStack.length === 0}>Previous</button>
            <span>Page {cursorBackStack.length + 1}</span>
            <button class="btn btn-outline btn-xs" onclick={goNext} disabled={!nextCursor}>Next</button>
          </nav>
        {/if}
      {/if}
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />

<style>
  .jobs-page {
    display: grid;
    gap: 1.5rem;
  }

  .jobs-page :global(.mb-5) {
    margin-bottom: 0;
  }

  .jobs-updated {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.72rem;
    margin: -1.15rem 0 -0.75rem;
    text-align: right;
  }

  .jobs-overview {
    display: grid;
    gap: 1.5rem;
    grid-template-columns: minmax(0, 1.6fr) minmax(14rem, 0.52fr);
    align-items: start;
  }

  .jobs-overview > * {
    min-width: 0;
  }

  .row-selected {
    background: color-mix(in oklab, var(--color-primary) 10%, var(--color-base-100));
  }

  .jobs-progress {
    background: color-mix(in oklab, var(--color-base-300) 70%, transparent);
    border-radius: 999px;
    display: block;
    height: 0.22rem;
    margin-top: 0.35rem;
    overflow: hidden;
  }

  .jobs-progress i {
    background: color-mix(in oklab, var(--color-primary) 75%, var(--color-base-content));
    display: block;
    height: 100%;
    width: var(--job-progress);
  }

  @media (max-width: 1100px) {
    .jobs-overview {
      grid-template-columns: 1fr;
    }
  }
</style>
