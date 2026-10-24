<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import {
    beginOwnedWatch,
    LiveSubscription,
    RefreshScheduler,
    type WatchAttempt,
  } from "$lib/console/live_refresh.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { type apis } from "trellis-web-generated";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import ColumnChart from "$lib/components/ColumnChart.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import MetricsLedger from "$lib/components/MetricsLedger.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { boundedNumber, errorMessage } from "$lib/format";
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
  import { errorText, friendlyDuration, jobStateLabel as plainJobState, sentenceName } from "$lib/console/plain_labels.ts";
  import { createServiceNames } from "$lib/console/service_names.svelte.ts";

  const names = createServiceNames();

  type Job = apis.jobs.QueryOutput["items"][number];
  type JobState = Job["state"];
  type ServiceInfo = apis.jobs.ListServicesOutput["items"][number];
  type MetricsWindow = apis.jobs.MetricsInput["window"];
  type Focus = "running-risk" | "running" | "action" | "completed" | "failed" | "dead" | "backlog";
  type JobPathname = `/admin/jobs/${string}` & {};

  const trellis = getTrellis();
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
  let watchAttempt: WatchAttempt<unknown> | null = null;
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

  const listTabs: Array<{ value: Focus; label: string }> = [
    { value: "action", label: "Needs attention" },
    { value: "running-risk", label: "Running" },
    { value: "backlog", label: "Waiting" },
    { value: "completed", label: "Finished" },
    { value: "failed", label: "Failed" },
    { value: "dead", label: "Failed for good" },
  ];

  function emptyText(): { title: string; description: string } {
    if (focus === "action") return { title: "Nothing needs attention", description: "Failed, stuck, and retrying jobs show up here." };
    if (focus === "running-risk" || focus === "running") return { title: "Nothing is running", description: "Jobs appear here while a service works on them." };
    if (focus === "backlog") return { title: "Nothing is waiting", description: "Jobs wait here until a service is free to run them." };
    if (focus === "dead") return { title: "No jobs failed for good", description: "Jobs that run out of tries wait here so you can run them again or dismiss them." };
    if (focus === "failed") return { title: "No failed jobs", description: "Jobs that fail without retrying show up here." };
    return { title: "No finished jobs", description: "Try a longer time range." };
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
        summaryError = null;
      } else {
        groups = [];
        stats = { byState: {}, total: 0n };
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

  function riskPriority(job: Job): number {
    if ((job.waitingOn?.length ?? 0) > 0) return 0;
    if (job.state === "retry") return 1;
    if (job.runtimeBand === "slow") return 2;
    return 3;
  }

  function jobNarrative(job: Job): string {
    const wait = job.waitingOn?.[0];
    if (job.state === "active" && wait) return `Waiting for ${wait.label ?? wait.target.label ?? sentenceName(wait.target.kind).toLowerCase()}`;
    if (job.state === "retry") return "Will try again soon";
    if (job.state === "stale") return "The service running it stopped responding";
    if (job.state === "failed" || job.state === "dead") return errorText(job.lastError) ?? "No reason was given.";
    if (job.state === "completed") return "Done";
    if (job.state === "pending") return "Waiting for a service to pick it up";
    return job.progress?.message ?? job.progress?.step ?? "Working";
  }

  function jobTypeLabel(type: string): string {
    return sentenceName(type);
  }

  function jobDuration(job: Job): string {
    if (job.state === "pending" || job.state === "retry") return `Waiting ${friendlyDuration(boundedNumber(job.queueAgeMs ?? 0n))}`;
    const runtime = friendlyDuration(boundedNumber(job.runtimeMs ?? 0n));
    return job.state === "active" ? `Running ${runtime}` : `Took ${runtime}`;
  }

  function jobProgress(job: Job): number {
    const current = job.progress?.current;
    const total = job.progress?.total;
    if (current === undefined || total === undefined || total <= 0n) return 0;
    return Math.min(100, Math.max(0, boundedNumber(current) / boundedNumber(total) * 100));
  }

  const ledgerItems = $derived([
    { id: "action", label: "Needs attention", value: overview.action, detail: overview.action > 0 ? [overview.failed + overview.dead > 0 ? `${overview.failed + overview.dead} failed` : "", overview.retrying > 0 ? `${overview.retrying} retrying` : "", overview.stale > 0 ? `${overview.stale} stuck` : ""].filter(Boolean).join(" · ") : "All good", tone: overview.action > 0 ? "error" as const : "success" as const, active: focus === "action", attention: overview.action > 0 },
    { id: "running", label: "Running", value: overview.running, detail: overview.slow > 0 ? `${overview.slow} slower than usual` : `${workerCount} ${workerCount === 1 ? "worker" : "workers"} ready`, tone: "success" as const, active: focus === "running" || focus === "running-risk" },
    { id: "backlog", label: "Waiting", value: overview.backlog, detail: "In line to run", tone: overview.backlog > 0 ? "warning" as const : "success" as const, active: focus === "backlog" },
    { id: "completed", label: "Finished", value: overview.processed.toLocaleString(), detail: overview.failureRate > 0 ? `${overview.failureRate.toFixed(1)}% failed` : "None failed", tone: "info" as const, active: focus === "completed" },
  ]);

  const chartPoints = $derived(
    (metrics?.buckets ?? []).map((bucket) => {
      const groups = selectedJobType ? bucket.groups.filter((group) => group.key === selectedJobType) : bucket.groups;
      const date = new Date(bucket.start);
      return {
        label: date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
        tick: metricsWindow === "7d" ? date.toLocaleDateString(undefined, { weekday: "short" }) : date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }),
        value: boundedNumber(groups.reduce((sum, group) => sum + group.completed, 0n)),
      };
    }),
  );

  function handleLedgerSelect(id: string) {
    if (id === "running") selectFocus("running-risk");
    else if (id === "action" || id === "completed" || id === "failed" || id === "dead" || id === "backlog") selectFocus(id);
  }

  function stopWatch() {
    const attempt = watchAttempt;
    watchAttempt = null;
    attempt?.controller.abort();
    attempt?.stream?.close?.();
    void subscription?.dispose();
    subscription = null;
    watchStatus = "Unavailable";
  }

  function startWatch() {
    stopWatch();
    const live = new LiveSubscription({
      subscribe: async () => {
        const attempt = beginOwnedWatch({
          open: (signal) =>
            trellis.jobsWatch({ includeInitial: false }, { signal }).orThrow(),
          onFrame: () => refreshScheduler.notify(),
          stillOwned: (): boolean => !disposed && watchAttempt === attempt,
          onUnexpectedEnd: (cause) => live.closed(cause),
        });
        watchAttempt = attempt;
        await attempt.ready;
      },
      unsubscribe: async () => {
        const attempt = watchAttempt;
        watchAttempt = null;
        attempt?.controller.abort();
        attempt?.stream?.close?.();
        await attempt?.pump;
      },
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
  <PageToolbar title="Jobs" description="Background work your services run, and anything that needs a decision.">
    {#snippet meta()}
      {#if lastUpdated}
        <span class={["badge badge-sm", watchStatus === "Live" ? "badge-success" : "badge-neutral"]}>{watchStatus === "Live" ? "Live" : "Not live"}</span>
        <span class="jobs-updated text-xs text-base-content/55">Updated {lastUpdated.toLocaleTimeString()}<span class="sr-only" aria-live="polite">{watchStatus}</span></span>
      {/if}
    {/snippet}
    {#snippet actions()}
      <div class="trellis-segment" role="group" aria-label="Time range">
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

  {#if error}
    <Notice variant="error" role="alert">Jobs couldn't be loaded. {error}</Notice>
  {:else if unavailableMessage}
    <Notice variant="info" role="status">{unavailableMessage} Jobs keep running while this view is unavailable.</Notice>
  {/if}

  {#if servicesError}
    <Notice variant="warning" role="status">Worker counts can't be shown: {servicesError}</Notice>
  {/if}

  {#if summaryError}
    <Notice variant="warning" role="status">Totals can't be shown: {summaryError}</Notice>
  {/if}

  {#if bulkUnknown.length > 0}
    <Notice variant="warning" role="status">
      It isn't known whether {bulkUnknown.length} {bulkUnknown.length === 1 ? "job was" : "jobs were"} cancelled. Check them before trying again.
      <button class="btn btn-ghost btn-xs" type="button" onclick={() => bulkUnknown = []}>Hide</button>
    </Notice>
  {/if}

  {#if !loading && !unavailableMessage}
    {#if !summaryError}
      <MetricsLedger ariaLabel="Jobs status summary" items={ledgerItems} onSelect={handleLedgerSelect} />
    {/if}

    <div class="jobs-overview">
      {#if !summaryError}
        <Panel title="Job types">
          {#snippet actions()}
            {#if selectedJobType}<button type="button" class="trellis-chip active" onclick={() => selectJobType(null)}>{jobTypeLabel(selectedJobType)} <span aria-hidden="true">×</span><span class="sr-only">Show all job types</span></button>{/if}
          {/snippet}
          {#if groups.length === 0}
            <p class="py-6 text-center text-sm text-base-content/60">No jobs in this period.</p>
          {:else}
            <ul class="type-list">
              {#each groups as group (group.key)}
                {@const selected = selectedJobType === group.key}
                <li>
                  <button type="button" class="type-row" class:selected aria-pressed={selected} title="Show only this job type" onclick={() => selectJobType(selected ? null : group.key)}>
                    <span class="min-w-0">
                      <span class="block truncate font-medium">{jobTypeLabel(group.label)}</span>
                      <span class="block truncate text-xs text-base-content/60">
                        {group.count.toLocaleString()} {group.count === 1n ? "job" : "jobs"}{(group.depth ?? 0n) > 0n ? ` · ${(group.depth ?? 0n).toLocaleString()} waiting` : ""}{group.oldestCreatedAt && (group.depth ?? 0n) > 0n ? ` · oldest ${friendlyDuration(Date.now() - new Date(group.oldestCreatedAt).getTime())}` : ""}
                      </span>
                    </span>
                    {#if group.failureRate !== undefined && group.failureRate > 0}
                      <span class="text-sm text-error tabular-nums">{(group.failureRate * 100).toFixed(1)}% failed</span>
                    {:else}
                      <span class="text-sm text-base-content/50">No failures</span>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
        </Panel>
      {/if}
      <Panel title="Finished over time">
        {#snippet actions()}<span class="text-sm text-base-content/60">{windowLabel}</span>{/snippet}
        {#if metricsLoading && !metrics}
          <LoadingState label="Loading job trends" />
        {:else if metricsError}
          <p class="py-10 text-center text-sm text-base-content/60">Trends can't be shown: {metricsError}</p>
        {:else if chartPoints.length === 0}
          <p class="py-10 text-center text-sm text-base-content/60">No jobs in this period.</p>
        {:else}
          <ColumnChart points={chartPoints} unit="jobs finished" />
        {/if}
      </Panel>
    </div>
  {/if}

  {#if !unavailableMessage}
    <Panel title="Jobs">
      {#snippet actions()}
        <div class="trellis-segment" role="group" aria-label="Which jobs">
          {#each listTabs as tab (tab.value)}
            <button type="button" class:active={focus === tab.value || (tab.value === "running-risk" && focus === "running")} aria-pressed={focus === tab.value} onclick={() => selectFocus(tab.value)}>{tab.label}</button>
          {/each}
        </div>
      {/snippet}
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
        <LoadingState label="Loading jobs" class="min-h-32" />
      {:else if jobs.length === 0}
        <EmptyState title={emptyText().title} description={emptyText().description} />
      {:else}
        <DataTable>
          <thead><tr>
            <th class="w-8">
              <input
                type="checkbox"
                class="checkbox checkbox-xs"
                aria-label="Select all jobs that can be cancelled"
                disabled={bulkBusy || selectableJobIds.length === 0}
                checked={selectableJobIds.length > 0 && selectableJobIds.every((id) => selectedJobs.has(id))}
                indeterminate={selectableJobIds.some((id) => selectedJobs.has(id)) && !selectableJobIds.every((id) => selectedJobs.has(id))}
                onchange={() => toggleAll(selectedJobs, selectableJobIds)}
              />
            </th>
            <th>Job</th><th>Status</th><th>What's happening</th><th>Time</th><th class="text-right">Tries</th></tr></thead>
          <tbody>
            {#each focusedJobs as job (job.id)}
              {@const progress = jobProgress(job)}
              {@const state = plainJobState(job)}
              <tr>
                <td>
                  {#if job.state === "active" || job.state === "pending" || job.state === "retry"}
                    <input
                      type="checkbox"
                      class="checkbox checkbox-xs"
                      aria-label={`Select ${jobTypeLabel(job.type)} job`}
                      disabled={bulkBusy}
                      checked={selectedJobs.has(job.id)}
                      onchange={() => toggleId(selectedJobs, job.id)}
                    />
                  {/if}
                </td>
                <td class="min-w-0">
                  <a class="link link-hover block max-w-xs truncate font-medium" href={resolve(jobRoute(job.id))}>{jobTypeLabel(job.type)}</a>
                  <span class="block max-w-xs truncate text-xs text-base-content/60">{names.participant(job.service)}</span>
                </td>
                <td><StatusBadge label={state.label} status={state.tone} /></td>
                <td class="max-w-md">
                  <span class={["block truncate text-sm", state.tone === "unhealthy" ? "text-error" : ""]} title={jobNarrative(job)}>{jobNarrative(job)}</span>
                  {#if progress > 0}<span class="jobs-progress"><i style:--job-progress={`${progress}%`}></i></span>{/if}
                </td>
                <td class="whitespace-nowrap text-sm tabular-nums">{jobDuration(job)}</td>
                <td class="text-right text-sm tabular-nums">{job.tries} of {job.maxTries}</td>
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
    gap: 1.25rem;
    grid-template-columns: minmax(0, 1fr);
  }

  .jobs-overview {
    display: grid;
    gap: 1.25rem;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    align-items: start;
  }

  .jobs-overview > :global(*) {
    min-width: 0;
  }

  .type-list > li {
    border-top: 1px solid var(--color-base-300);
  }

  .type-list > li:first-child {
    border-top: 0;
  }

  .type-row {
    align-items: center;
    background: transparent;
    border: 0;
    cursor: pointer;
    display: flex;
    gap: 1rem;
    justify-content: space-between;
    padding: 0.7rem 0.25rem;
    text-align: left;
    width: 100%;
  }

  .type-row:hover {
    background: color-mix(in oklab, var(--color-base-200) 60%, transparent);
  }

  .type-row.selected {
    background: color-mix(in oklab, var(--color-primary) 7%, transparent);
  }

  .jobs-progress {
    background: color-mix(in oklab, var(--color-base-300) 70%, transparent);
    border-radius: 999px;
    display: block;
    height: 0.25rem;
    margin-top: 0.35rem;
    overflow: hidden;
  }

  .jobs-progress i {
    background: var(--color-primary);
    display: block;
    height: 100%;
    width: var(--job-progress);
  }

  @media (max-width: 1100px) {
    .jobs-overview {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
