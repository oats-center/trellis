<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import { afterNavigate, goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, tick, untrack } from "svelte";
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
  type Focus = "all" | "running-risk" | "running" | "action" | "completed" | "failed" | "dead" | "backlog";
  type View = "overview" | "log";
  type Measure = "completed" | "submitted" | "started" | "failed" | "retried";
  type TypeStats = apis.jobs.MetricsOutput["summary"][number];
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
  /** A link that names which jobs to show opens the list; otherwise the overview. */
  let view = $state<View>(viewOf(page.url));
  let measure = $state<Measure>("completed");
  const views: Array<{ value: View; label: string }> = [
    { value: "overview", label: "Overview" },
    { value: "log", label: "Job list" },
  ];
  const measures: Array<{ value: Measure; label: string; unit: string }> = [
    { value: "completed", label: "Finished", unit: "jobs finished" },
    { value: "submitted", label: "Created", unit: "jobs created" },
    { value: "started", label: "Started", unit: "tries started" },
    { value: "failed", label: "Failed", unit: "jobs failed" },
    { value: "retried", label: "Retried", unit: "retries" },
  ];

  afterNavigate(() => {
    view = viewOf(page.url);
    const value = page.url.searchParams.get("focus");
    if (value === handledFocusParam) return;
    handledFocusParam = value;
    const next = asFocus(value);
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

  function viewOf(url: URL): View {
    return url.searchParams.get("view") === "list" || url.searchParams.has("focus") ? "log" : "overview";
  }

  async function showView(next: View): Promise<void> {
    if (next === view) return;
    view = next;
    const base = resolve("/admin/jobs");
    await goto(next === "log" ? `${base}?view=list` : base, { noScroll: true, keepFocus: true });
    await tick();
  }

  /** Opens the list showing one kind of job. */
  function openFocus(value: Focus) {
    selectedJobType = null;
    selectFocus(value);
    void showView("log");
  }

  /** Opens the list showing every job of one type. */
  function openJobType(key: string) {
    selectedJobType = key;
    selectFocus("all");
    void showView("log");
  }

  function asFocus(value: string | null): Focus | null {
    if (value === "all" || value === "running-risk" || value === "running" || value === "action" || value === "completed" || value === "failed" || value === "dead" || value === "backlog") return value;
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

  function focusStates(value: Focus): JobState[] | undefined {
    if (value === "all") return undefined;
    if (value === "running-risk") return ["active", "retry"];
    if (value === "running") return ["active"];
    if (value === "action") return ["retry", "failed", "dead", "stale"];
    if (value === "completed") return ["completed"];
    if (value === "failed") return ["failed"];
    if (value === "dead") return ["dead"];
    return ["pending", "retry"];
  }

  const listTabs: Array<{ value: Focus; label: string }> = [
    { value: "all", label: "All" },
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
    if (focus === "all") return { title: "No jobs", description: "Jobs your services create show up here." };
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
    { id: "action", label: "Needs attention", value: overview.action, detail: overview.action > 0 ? [overview.failed + overview.dead > 0 ? `${overview.failed + overview.dead} failed` : "", overview.retrying > 0 ? `${overview.retrying} retrying` : "", overview.stale > 0 ? `${overview.stale} stuck` : ""].filter(Boolean).join(" · ") : "All good", tone: overview.action > 0 ? "error" as const : "success" as const, active: view === "log" && focus === "action", attention: overview.action > 0 },
    { id: "running", label: "Running", value: overview.running, detail: overview.slow > 0 ? `${overview.slow} slower than usual` : `${workerCount} ${workerCount === 1 ? "worker" : "workers"} ready`, tone: "success" as const, active: view === "log" && (focus === "running" || focus === "running-risk") },
    { id: "backlog", label: "Waiting", value: overview.backlog, detail: "In line to run", tone: overview.backlog > 0 ? "warning" as const : "success" as const, active: view === "log" && focus === "backlog" },
    { id: "completed", label: "Finished", value: overview.processed.toLocaleString(), detail: overview.failureRate > 0 ? `${overview.failureRate.toFixed(1)}% failed` : "None failed", tone: "info" as const, active: view === "log" && focus === "completed" },
  ]);

  const chartPoints = $derived(
    (metrics?.buckets ?? []).map((bucket) => {
      const groups = selectedJobType ? bucket.groups.filter((group) => group.key === selectedJobType) : bucket.groups;
      const date = new Date(bucket.start);
      return {
        label: date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
        tick: metricsWindow === "7d" ? date.toLocaleDateString(undefined, { weekday: "short" }) : date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }),
        value: boundedNumber(groups.reduce((sum, group) => sum + measureCount(group), 0n)),
      };
    }),
  );

  function handleLedgerSelect(id: string) {
    if (id === "running") openFocus("running-risk");
    else if (id === "action" || id === "completed" || id === "failed" || id === "dead" || id === "backlog") openFocus(id);
  }

  function measureCount(group: apis.jobs.MetricsOutput["buckets"][number]["groups"][number]): bigint {
    if (measure === "submitted") return group.submitted;
    if (measure === "started") return group.started;
    if (measure === "failed") return group.failed + group.dead;
    if (measure === "retried") return group.retried;
    return group.completed;
  }

  const measureOption = $derived(measures.find((option) => option.value === measure) ?? measures[0]);
  const typeStats = $derived(
    [...(metrics?.summary ?? [])].sort((a, b) => boundedNumber(b.total - a.total) || a.label.localeCompare(b.label)),
  );
  /** Every outcome in the period, summed over its intervals and job types. */
  const totals = $derived.by(() => {
    const sum = { submitted: 0n, started: 0n, completed: 0n, failed: 0n, dead: 0n, retried: 0n, cancelled: 0n, dismissed: 0n };
    for (const bucket of metrics?.buckets ?? [])
      for (const group of bucket.groups) {
        sum.submitted += group.submitted;
        sum.started += group.started;
        sum.completed += group.completed;
        sum.failed += group.failed;
        sum.dead += group.dead;
        sum.retried += group.retried;
        sum.cancelled += group.cancelled;
        sum.dismissed += group.dismissed;
      }
    return sum;
  });
  const outcomeRows = $derived.by(() => {
    const rows: Array<{ label: string; detail: string; count: bigint; focus?: Focus; tone: "ok" | "problem" | "neutral" }> = [
      { label: "Finished", detail: "Done without an error", count: totals.completed, focus: "completed", tone: "ok" },
      { label: "Failed", detail: "Stopped with an error and won't be tried again", count: totals.failed, focus: "failed", tone: "problem" },
      { label: "Failed for good", detail: "Ran out of tries; waiting for you to retry or dismiss", count: totals.dead, focus: "dead", tone: "problem" },
      { label: "Retried", detail: "Tried again after a try failed", count: totals.retried, focus: "action", tone: "neutral" },
      { label: "Cancelled", detail: "Stopped before finishing", count: totals.cancelled, tone: "neutral" },
      { label: "Dismissed", detail: "Failed for good, then set aside", count: totals.dismissed, tone: "neutral" },
    ];
    return rows.filter((row) => row.count > 0n || row.tone !== "neutral");
  });
  const outcomeTotal = $derived(outcomeRows.reduce((sum, row) => sum + boundedNumber(row.count), 0));
  /** Figures for the period that the summary cards don't already give. */
  const figures = $derived.by(() => {
    if (!metrics) return [];
    const weighted = (pick: (item: TypeStats) => { count: bigint; p50Ms?: bigint }) => {
      let count = 0;
      let total = 0;
      for (const item of metrics?.summary ?? []) {
        const latency = pick(item);
        if (latency.p50Ms === undefined || latency.count === 0n) continue;
        count += boundedNumber(latency.count);
        total += boundedNumber(latency.p50Ms) * boundedNumber(latency.count);
      }
      return count > 0 ? total / count : undefined;
    };
    const slowest = (metrics.summary ?? []).reduce<TypeStats | undefined>(
      (worst, item) => ((item.runtime.p95Ms ?? 0n) > (worst?.runtime.p95Ms ?? 0n) ? item : worst),
      undefined,
    );
    const run = weighted((item) => item.runtime);
    const wait = weighted((item) => item.queueWait);
    const ended = boundedNumber(totals.completed + totals.failed + totals.dead);
    return [
      { label: "Created", value: totals.submitted.toLocaleString(), detail: `${totals.started.toLocaleString()} ${totals.started === 1n ? "try" : "tries"} started` },
      { label: "Succeeded", value: ended > 0 ? `${((boundedNumber(totals.completed) / ended) * 100).toFixed(1)}%` : "—", detail: `of ${ended.toLocaleString()} that ended` },
      { label: "Typical run", value: run === undefined ? "—" : friendlyDuration(run), detail: slowest?.runtime.p95Ms ? `${jobTypeLabel(slowest.label)} is slowest: ${friendlyDuration(boundedNumber(slowest.runtime.p95Ms))} for most` : undefined },
      { label: "Typical wait", value: wait === undefined ? "—" : friendlyDuration(wait), detail: "Before a service picks a job up" },
      { label: "Workers", value: workerCount.toLocaleString(), detail: `in ${services.length} ${services.length === 1 ? "service" : "services"}` },
    ];
  });

  /** One service's workers, grouped by the job type they run. */
  function workerTypes(service: ServiceInfo): Array<{ type: string; count: number; concurrency: number }> {
    const byType = new Map<string, { type: string; count: number; concurrency: number }>();
    for (const worker of service.workers) {
      const entry = byType.get(worker.jobType) ?? { type: worker.jobType, count: 0, concurrency: 0 };
      entry.count += 1;
      entry.concurrency += Number(worker.concurrency ?? 1);
      byType.set(worker.jobType, entry);
    }
    return [...byType.values()].sort((a, b) => a.type.localeCompare(b.type));
  }

  function latencyLabel(latency: { count: bigint; p50Ms?: bigint; p95Ms?: bigint }): string {
    if (latency.count === 0n || latency.p50Ms === undefined) return "—";
    const typical = friendlyDuration(boundedNumber(latency.p50Ms));
    return latency.p95Ms === undefined ? typical : `${typical} · ${friendlyDuration(boundedNumber(latency.p95Ms))}`;
  }

  function share(count: bigint, total: number): number {
    return total > 0 ? boundedNumber(count) / total : 0;
  }

  function shareLabel(count: bigint, total: number): string {
    const value = share(count, total) * 100;
    return value >= 10 || value === 0 ? `${Math.round(value)}%` : `${value.toFixed(1)}%`;
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

  {#if !unavailableMessage}
    <div class="trellis-segment jobs-views" role="tablist" aria-label="Jobs view">
      {#each views as option (option.value)}
        <button type="button" role="tab" aria-selected={view === option.value} class:active={view === option.value} onclick={() => showView(option.value)}>{option.label}</button>
      {/each}
    </div>
  {/if}

  {#if !loading && !unavailableMessage && !summaryError}
    <MetricsLedger ariaLabel="Jobs status summary" items={ledgerItems} onSelect={handleLedgerSelect} />
  {/if}

  {#if view === "overview" && !loading && !unavailableMessage}
    <div class="jobs-overview">
      <Panel title="Activity">
        {#snippet actions()}<span class="text-sm text-base-content/60">{windowLabel}</span>{/snippet}
        <div class="trellis-segment measure-segment" role="group" aria-label="Measure">
          {#each measures as option (option.value)}
            <button type="button" class:active={measure === option.value} aria-pressed={measure === option.value} onclick={() => { measure = option.value; }}>{option.label}</button>
          {/each}
        </div>
        {#if metricsLoading && !metrics}
          <LoadingState label="Loading job trends" />
        {:else if metricsError}
          <p class="py-10 text-center text-sm text-base-content/60">Trends can't be shown: {metricsError}</p>
        {:else if chartPoints.every((point) => point.value === 0)}
          <p class="py-10 text-center text-sm text-base-content/60">None in this period.</p>
        {:else}
          <ColumnChart points={chartPoints} unit={measureOption.unit} />
        {/if}
      </Panel>
      <Panel title="In this period">
        {#if !metrics}
          <p class="text-sm text-base-content/60">{metricsLoading ? "Loading…" : "Not available."}</p>
        {:else}
          <dl class="figures">
            {#each figures as figure (figure.label)}
              <div><dt>{figure.label}</dt><dd>{figure.value}</dd>{#if figure.detail}<small>{figure.detail}</small>{/if}</div>
            {/each}
          </dl>
        {/if}
      </Panel>
    </div>

    <Panel title="Job types">
      {#snippet actions()}<span class="text-sm text-base-content/60">{windowLabel}</span>{/snippet}
      {#if !metrics}
        <p class="text-sm text-base-content/60">{metricsLoading ? "Loading…" : `Not available${metricsError ? `: ${metricsError}` : "."}`}</p>
      {:else if typeStats.length === 0}
        <p class="py-6 text-center text-sm text-base-content/60">No jobs in this period.</p>
      {:else}
        <p class="mb-2 text-sm text-base-content/60">Choose one to see its jobs. Times are typical, with the slowest one in twenty after them.</p>
        <DataTable>
          <thead><tr><th>Job type</th><th class="text-right">Jobs</th><th class="text-right">Waiting</th><th class="text-right">Failed</th><th class="hidden text-right md:table-cell">Wait</th><th class="hidden text-right md:table-cell">Run</th></tr></thead>
          <tbody>
            {#each typeStats as item (item.key)}
              {@const failed = (item.failed ?? 0n) + (item.dead ?? 0n)}
              <tr class="type-row-link" onclick={() => openJobType(item.key)}>
                <td class="min-w-0">
                  <button type="button" class="link link-hover block max-w-xs truncate text-left font-medium" onclick={(event) => { event.stopPropagation(); openJobType(item.key); }}>{jobTypeLabel(item.label)}</button>
                  {#if item.oldestCreatedAt && (item.queued ?? 0n) > 0n}<span class="block text-xs text-base-content/60">Oldest waiting {friendlyDuration(Date.now() - new Date(item.oldestCreatedAt).getTime())}</span>{/if}
                </td>
                <td class="text-right tabular-nums">{item.total.toLocaleString()}</td>
                <td class="text-right tabular-nums">{(item.queued ?? 0n).toLocaleString()}</td>
                <td class={["text-right tabular-nums", failed > 0n ? "text-error" : "text-base-content/50"]}>{failed > 0n ? `${failed.toLocaleString()} (${shareLabel(failed, boundedNumber(item.total))})` : "None"}</td>
                <td class="hidden text-right text-sm tabular-nums md:table-cell">{latencyLabel(item.queueWait)}</td>
                <td class="hidden text-right text-sm tabular-nums md:table-cell">{latencyLabel(item.runtime)}</td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
      {/if}
    </Panel>

    <div class="jobs-pair">
      <Panel title="Outcomes">
        {#if !metrics}
          <p class="text-sm text-base-content/60">{metricsLoading ? "Loading…" : "Not available."}</p>
        {:else}
          <p class="mb-2 text-sm text-base-content/60">How jobs ended in this period.</p>
          <div class="bar-list">
            {#each outcomeRows as row (row.label)}
              {#if row.focus}
                <button type="button" title="Show these jobs" onclick={() => openFocus(row.focus ?? "all")}>
                  <span><strong>{row.label}</strong><small>{row.detail}</small></span>
                  <b>{row.count.toLocaleString()} <small>{shareLabel(row.count, outcomeTotal)}</small></b>
                  <i class:problem={row.tone === "problem"} style={`--width: ${share(row.count, outcomeTotal) * 100}%`}></i>
                </button>
              {:else}
                <div>
                  <span><strong>{row.label}</strong><small>{row.detail}</small></span>
                  <b>{row.count.toLocaleString()} <small>{shareLabel(row.count, outcomeTotal)}</small></b>
                  <i style={`--width: ${share(row.count, outcomeTotal) * 100}%`}></i>
                </div>
              {/if}
            {/each}
          </div>
        {/if}
      </Panel>
      <Panel title="Workers">
        {#if servicesError}
          <p class="text-sm text-base-content/60">Workers can't be shown: {servicesError}</p>
        {:else if services.length === 0}
          <EmptyState title="No workers" description="Services that run jobs show up here while they're connected." />
        {:else}
          <ul class="worker-list">
            {#each services as service (service.name)}
              <li>
                <div class="flex items-center justify-between gap-3">
                  <span class="truncate font-medium">{names.participant(service.name)}</span>
                  <StatusBadge label={service.healthy ? "Healthy" : "Not healthy"} status={service.healthy ? "healthy" : "unhealthy"} />
                </div>
                <ul class="mt-1 text-sm text-base-content/70">
                  {#each workerTypes(service) as entry (entry.type)}
                    <li>{jobTypeLabel(entry.type)} · {entry.count} {entry.count === 1 ? "worker" : "workers"}, up to {entry.concurrency} at once</li>
                  {/each}
                </ul>
              </li>
            {/each}
          </ul>
        {/if}
      </Panel>
    </div>
  {/if}

  {#if view === "log" && !unavailableMessage}
    <Panel title="Jobs">
      {#snippet actions()}
        <div class="flex flex-wrap items-center gap-2">
          <div class="trellis-segment" role="group" aria-label="Which jobs">
            {#each listTabs as tab (tab.value)}
              <button type="button" class:active={focus === tab.value || (tab.value === "running-risk" && focus === "running")} aria-pressed={focus === tab.value} onclick={() => selectFocus(tab.value)}>{tab.label}</button>
            {/each}
          </div>
          {#if groups.length > 0 || selectedJobType}
            <label class="select select-sm w-48">
              <span class="sr-only">Job type</span>
              <select value={selectedJobType ?? ""} onchange={(event) => selectJobType(event.currentTarget.value || null)}>
                <option value="">Every job type</option>
                {#if selectedJobType && !groups.some((group) => group.key === selectedJobType)}<option value={selectedJobType}>{jobTypeLabel(selectedJobType)}</option>{/if}
                {#each groups as group (group.key)}
                  <option value={group.key}>{jobTypeLabel(group.label)}</option>
                {/each}
              </select>
            </label>
          {/if}
        </div>
      {/snippet}
      {#if selectedJobType}
        <div class="chip-row" aria-label="Active filter">
          <button type="button" class="trellis-chip active" onclick={() => selectJobType(null)}>{jobTypeLabel(selectedJobType)} <span aria-hidden="true">×</span><span class="sr-only">Show every job type</span></button>
        </div>
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

  .jobs-views {
    justify-self: start;
  }

  .measure-segment {
    margin-bottom: 0.75rem;
    max-width: 100%;
    overflow-x: auto;
  }

  .jobs-overview {
    display: grid;
    gap: 1.25rem;
    grid-template-columns: minmax(0, 2fr) minmax(16rem, 1fr);
    align-items: stretch;
  }

  .jobs-pair {
    display: grid;
    gap: 1.25rem;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    align-items: start;
  }

  .jobs-overview > :global(*),
  .jobs-pair > :global(*) {
    min-width: 0;
  }

  .figures {
    display: grid;
    gap: 1.25rem 1rem;
    grid-template-columns: repeat(auto-fill, minmax(9rem, 1fr));
  }

  .figures dt,
  .figures small {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
  }

  .figures dt {
    font-size: 0.8125rem;
  }

  .figures dd {
    font-size: 1.25rem;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    line-height: 1.3;
  }

  .figures small {
    display: block;
    font-size: 0.75rem;
  }

  .type-row-link {
    cursor: pointer;
  }

  .type-row-link:hover {
    background: color-mix(in oklab, var(--color-base-200) 55%, transparent);
  }

  .bar-list {
    display: grid;
    gap: 0.35rem;
  }

  .bar-list > button,
  .bar-list > div {
    background: transparent;
    border: 0;
    border-radius: 0.5rem;
    display: grid;
    gap: 0.2rem 0.5rem;
    grid-template-columns: minmax(0, 1fr) auto;
    margin: 0 -0.35rem;
    padding: 0.4rem 0.35rem;
    text-align: left;
    width: calc(100% + 0.7rem);
  }

  .bar-list > button {
    cursor: pointer;
  }

  .bar-list > button:hover {
    background: color-mix(in oklab, var(--color-base-200) 72%, transparent);
  }

  .bar-list strong,
  .bar-list span > small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bar-list strong {
    font-size: 0.875rem;
    font-weight: 500;
  }

  .bar-list span > small,
  .bar-list b small {
    color: color-mix(in oklab, var(--color-base-content) 58%, transparent);
    font-size: 0.75rem;
    font-weight: 400;
  }

  .bar-list b {
    font-size: 0.875rem;
    font-variant-numeric: tabular-nums;
    font-weight: 600;
    text-align: right;
  }

  .bar-list i {
    background: color-mix(in oklab, var(--color-primary) 55%, var(--color-base-300));
    border-radius: 999px;
    grid-column: 1 / -1;
    height: 0.25rem;
    width: var(--width);
  }

  .bar-list i.problem {
    background: color-mix(in oklab, var(--color-error) 65%, var(--color-base-300));
  }

  .worker-list > li {
    border-top: 1px solid var(--color-base-300);
    padding: 0.7rem 0;
  }

  .worker-list > li:first-child {
    border-top: 0;
    padding-top: 0;
  }

  .chip-row {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 0.4rem;
    margin-bottom: 0.75rem;
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
    .jobs-overview,
    .jobs-pair {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
