<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, onMount } from "svelte";
  import { RefreshScheduler } from "$lib/console/live_refresh.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import JobEventTimeline from "$lib/components/JobEventTimeline.svelte";
  import { jobTimelineEventsForAttempt } from "$lib/job_event_timeline";
  import JsonTree from "$lib/components/JsonTree.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import {
    boundedNumber,
    compactDuration,
    errorMessage,
    formatDate,
    jobStateStatus,
    jsonBlock,
  } from "$lib/format";
  import {
    cancelJob,
    dismissDlqJob,
    loadJobDetailData,
    replayDlqJob,
    retryJob,
    type JobInspection,
  } from "$lib/jobs_page.ts";
  import { loadJobsMetrics, type JobsMetrics } from "$lib/jobs_metrics.ts";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  type Inspection = JobInspection;
  type Job = Inspection["job"];
  type WaitEdge = NonNullable<Job["waitingOn"]>[number];

  const jobId = $derived(page.params.jobid);
  const currentJobId = $derived(jobId ?? "");
  let loading = $state(true);
  let actionBusy = $state<string | null>(null);
  let error = $state<string | null>(null);
  let unavailableMessage = $state<string | null>(null);
  let inspection = $state.raw<Inspection | undefined>(undefined);
  let loadedJobId = $state<string | null>(null);
  let metrics = $state<JobsMetrics | null>(null);
  let metricsUnavailable = $state(false);
  let metricsSequence = 0;
  let copyFlash = $state<string | null>(null);
  let copyFlashTimer: ReturnType<typeof setTimeout> | undefined;
  let watchController: AbortController | undefined;
  let watchedJobId: string | null = null;
  /** True only while a usable watch is actually established. */
  let watchLive = $state(false);
  /** Set on disposal so no read, timer, or feed work runs afterwards. */
  let disposed = false;
  let loadSequence = 0;
  let activeAttemptIndex = $state(0);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const job = $derived(inspection?.job);
  const attempts = $derived(inspection?.attempts ?? []);
  const errors = $derived(inspection?.errors ?? []);
  const related = $derived(inspection?.related ?? []);
  const timeline = $derived(inspection?.timeline ?? []);
  const activeWaits = $derived(job?.state === "active" ? job.waitingOn ?? [] : []);
  const selectedTimeline = $derived.by(() => {
    const tryNumber = selectedAttempt?.try;
    if (tryNumber === undefined) return timeline;
    return jobTimelineEventsForAttempt(timeline, tryNumber);
  });
  const canCancel = $derived(job?.state === "pending" || job?.state === "retry" || job?.state === "active");
  const canRetry = $derived(job?.state === "failed");
  const canDlq = $derived(job?.state === "dead");

  const sortedAttempts = $derived([...attempts].sort((a, b) => boundedNumber(a.try - b.try)));
  const selectedAttempt = $derived(
    sortedAttempts[Math.min(activeAttemptIndex, Math.max(0, sortedAttempts.length - 1))] ?? null,
  );

  function canShowJobName(j: typeof job): j is typeof job & { type: string } {
    return Boolean(j && typeof j.type === "string" && j.type.length > 0);
  }

  const jobDeploymentId = $derived.by(() => {
    if (!job) return "";
    const svc = (job as { service?: unknown }).service;
    return typeof svc === "string" && svc.length > 0 ? svc : "";
  });

  const stateStatus = jobStateStatus;

  function attemptDurationLabel(attempt: { startedAt?: string; endedAt?: string; createdAt?: string; updatedAt?: string } | null | undefined): string {
    const start = attempt?.startedAt ?? attempt?.createdAt;
    const end = attempt?.endedAt ?? attempt?.updatedAt ?? Date.now();
    if (!start) return UNSET;
    const startMs = new Date(start).getTime();
    const endMs = typeof end === "number" ? end : new Date(end).getTime();
    if (Number.isNaN(startMs) || Number.isNaN(endMs) || endMs < startMs) return UNSET;
    return compactDuration(endMs - startMs);
  }

  const showOutput = $derived(errors.length > 0 || (job?.result !== undefined && job?.result !== null));
  const outputIsError = $derived(errors.length > 0);

  const relatedReasonLabels = {
    trace: "Same trace",
    parent: "Same parent",
    root: "Same root",
    operation: "Same operation",
    concurrency: "Same concurrency key",
    wait: "Wait edge",
  } as const;

  function relatedReasonLabel(reason: string): string | null {
    if (reason === "trace") return relatedReasonLabels.trace;
    if (reason === "parent") return relatedReasonLabels.parent;
    if (reason === "root") return relatedReasonLabels.root;
    if (reason === "operation") return relatedReasonLabels.operation;
    if (reason === "concurrency") return relatedReasonLabels.concurrency;
    if (reason === "wait") return relatedReasonLabels.wait;
    return null;
  }

  function waitTargetLabel(edge: WaitEdge): string {
    return edge.target.label ?? edge.label ?? edge.target.operation ?? edge.target.type ?? edge.target.key ?? edge.target.operationId ?? edge.target.id ?? edge.target.kind;
  }

  function waitDurationLabel(edge: WaitEdge): string {
    const startedMs = new Date(edge.startedAt).getTime();
    if (Number.isNaN(startedMs)) return UNSET;
    return compactDuration(Date.now() - startedMs);
  }

  const relatedWithReason = $derived.by(() => {
    return related
      .map((candidate) => {
        const matchedBy = "matchedBy" in candidate ? candidate.matchedBy : undefined;
        const label = typeof matchedBy === "string" && matchedBy.length > 0 ? relatedReasonLabel(matchedBy) : null;
        return { job: candidate, match: { label: label ?? "" } };
      })
      .filter((entry) => entry.match.label.length > 0);
  });

  const meaningfulRelated = $derived(relatedWithReason);

  const jobRuntime = $derived.by(() => {
    if (!job) return UNSET;
    const start = job.startedAt ?? job.createdAt;
    if (!start) return UNSET;
    const isTerminal = job.state === "completed" || job.state === "failed" || job.state === "dead" || job.state === "cancelled" || job.state === "dismissed" || job.state === "skipped" || job.state === "expired";

    const end = isTerminal
      ? (job.completedAt ?? job.updatedAt ?? new Date().toISOString())
      : new Date().toISOString();
    const startMs = new Date(start).getTime();
    const endMs = new Date(end).getTime();
    if (Number.isNaN(startMs) || Number.isNaN(endMs) || endMs < startMs) return UNSET;
    return compactDuration(endMs - startMs);
  });

  const UNSET = "–";
  const NEWLINE = "\n";

  function parseErrorPayload(message: string | undefined): Record<string, unknown> | null {
    if (!message) return null;
    const trimmed = message.trim();
    if (!trimmed.startsWith("{") || !trimmed.endsWith("}")) return null;
    try {
      const parsed = JSON.parse(trimmed);
      if (parsed && typeof parsed === "object") return parsed as Record<string, unknown>;
    } catch {
      // not JSON; fall through
    }
    return null;
  }

  async function copyText(key: string, value: unknown) {
    try {
      await navigator.clipboard.writeText(jsonBlock(value));
    } catch {
      return;
    }
    copyFlash = key;
    if (copyFlashTimer) clearTimeout(copyFlashTimer);
    copyFlashTimer = setTimeout(() => {
      copyFlash = null;
      copyFlashTimer = undefined;
    }, 1200);
  }

  async function load(id = currentJobId, showLoading = true) {
    const sequence = ++loadSequence;
    loadedJobId = id;
    if (watchedJobId !== id) stopJobsWatch();
    if (showLoading) loading = true;
    error = null;
    unavailableMessage = null;

    try {
      const data = await loadJobDetailData({
        inspect: (input) => trellis.jobsInspect(input),
      }, id);
      if (sequence !== loadSequence || disposed) return;
      unavailableMessage = data.available ? null : data.message ?? "Jobs admin runtime is unavailable.";
      inspection = data.inspection;
      const attemptCount = data.inspection?.attempts.length ?? 0;
      activeAttemptIndex = attemptCount > 0 ? attemptCount - 1 : 0;
      if (data.available) ensureJobsWatch(id);
      void loadJobMetrics();
    } catch (e) {
      if (sequence !== loadSequence || disposed) return;
      error = errorMessage(e);
      unavailableMessage = null;
      inspection = undefined;
    } finally {
      if (showLoading && sequence === loadSequence && !disposed) loading = false;
    }
  }

  function loadCurrentJobIfNeeded() {
    if (!currentJobId || currentJobId === loadedJobId) return;
    activeAttemptIndex = 0;
    void load(currentJobId);
  }

  async function runAction(name: "cancel" | "retry" | "replay" | "dismiss") {
    const actionJobId = job?.id ?? currentJobId;
    if (disposed || actionJobId !== currentJobId) return;
    actionBusy = name;
    error = null;
    try {
      if (name === "cancel") {
        await cancelJob({ action: (input) => trellis.cancel(input) }, actionJobId);
      } else if (name === "retry") {
        await retryJob({ action: (input) => trellis.retry(input) }, actionJobId);
      } else if (name === "replay") {
        await replayDlqJob({ action: (input) => trellis.replayDlq(input) }, actionJobId);
      } else {
        await dismissDlqJob({ action: (input) => trellis.dismissDlq(input) }, actionJobId);
      }
      if (!disposed && currentJobId === actionJobId) await load(actionJobId);
    } catch (e) {
      if (!disposed && currentJobId === actionJobId) error = classifyMutationError(e).kind === "unknown"
        ? "Job action outcome unknown. Inspect this job before attempting another action."
        : errorMessage(e);
    } finally {
      if (!disposed) actionBusy = null;
    }
  }

  async function guardedAction(name: "cancel" | "dismiss") {
    const actionJobId = job?.id ?? currentJobId;
    const confirmed = await confirmationModal?.confirm(
      name === "dismiss"
        ? {
            title: "Dismiss dead-lettered job?",
            message: "The job record is discarded and cannot be recovered.",
            confirmLabel: "Dismiss DLQ",
            targetLabel: "Job",
            targetName: actionJobId,
            expectedValue: actionJobId,
          }
        : {
            title: "Cancel job?",
            message: "Cancellation stops the current attempt. Work already committed is not rolled back.",
            confirmLabel: "Cancel job",
            targetLabel: "Job",
            targetName: actionJobId,
          },
    );
    if (!confirmed || disposed || actionJobId !== currentJobId || job?.id !== actionJobId) return;
    await runAction(name);
  }

  const watchScheduler = new RefreshScheduler({
    onRefresh: () => {
      const id = watchedJobId;
      if (id === null || disposed) return Promise.resolve();
      return load(id, false);
    },
    onRefreshError: (cause) => {
      error = errorMessage(cause);
    },
  });

  function stopJobsWatch() {
    watchController?.abort();
    watchController = undefined;
    watchedJobId = null;
    watchLive = false;
  }

  async function loadJobMetrics() {
    const j = job;
    if (!j) return;
    const sequence = ++metricsSequence;
    try {
      const payload = await loadJobsMetrics(
        { metrics: (request) => trellis.jobsMetrics(request) },
        {
          groupBy: "type",
          service: j.service,
          step: "5m",
          window: "1h",
        },
      );
      if (sequence !== metricsSequence || disposed) return;
      if (payload.available && payload.metrics) {
        metrics = payload.metrics;
        metricsUnavailable = false;
      } else {
        metrics = null;
        metricsUnavailable = true;
      }
    } catch {
      if (sequence !== metricsSequence || disposed) return;
      metrics = null;
      metricsUnavailable = false;
    }
  }

  const runtimeBaseline = $derived.by(() => {
    if (!job || !metrics) return null;
    const group = metrics.summary.find((g) => g.key === job.type);
    if (!group || !group.runtime || group.runtime.count === 0n) return null;
    return {
      p50: group.runtime.p50Ms == null ? null : boundedNumber(group.runtime.p50Ms),
      p95: group.runtime.p95Ms == null ? null : boundedNumber(group.runtime.p95Ms),
      max: group.runtime.maxMs == null ? null : boundedNumber(group.runtime.maxMs),
      count: group.runtime.count,
    };
  });

  const queueWaitBaseline = $derived.by(() => {
    if (!job || !metrics) return null;
    const group = metrics.summary.find((g) => g.key === job.type);
    if (!group || !group.queueWait || group.queueWait.count === 0n) return null;
    return {
      p50: group.queueWait.p50Ms == null ? null : boundedNumber(group.queueWait.p50Ms),
      p95: group.queueWait.p95Ms == null ? null : boundedNumber(group.queueWait.p95Ms),
      max: group.queueWait.maxMs == null ? null : boundedNumber(group.queueWait.maxMs),
      count: group.queueWait.count,
    };
  });

  const jobRuntimeMs = $derived.by(() => {
    if (!job) return null;
    const start = job.startedAt ?? job.createdAt;
    if (!start) return null;
    const isTerminal = job.state === "completed" || job.state === "failed" || job.state === "dead" || job.state === "cancelled" || job.state === "dismissed" || job.state === "skipped" || job.state === "expired";

    const end = isTerminal
      ? (job.completedAt ?? job.updatedAt ?? null)
      : new Date().toISOString();
    if (!end) return null;
    const startMs = new Date(start).getTime();
    const endMs = new Date(end).getTime();
    if (Number.isNaN(startMs) || Number.isNaN(endMs) || endMs < startMs) return null;
    return endMs - startMs;
  });

  const jobQueueWaitMs = $derived.by(() => {
    if (!job) return null;
    if (!job.startedAt) return null;
    const startMs = new Date(job.createdAt).getTime();
    const queuedMs = new Date(job.startedAt).getTime();
    if (Number.isNaN(startMs) || Number.isNaN(queuedMs) || queuedMs < startMs) return null;
    return queuedMs - startMs;
  });

  function runtimeTone(valueMs: number, p50: number | null, p95: number | null): "fast" | "normal" | "slow" {
    if (p95 != null && valueMs > p95) return "slow";
    if (p50 != null && valueMs > p50) return "normal";
    return "fast";
  }

  function ratio(valueMs: number, baseline: { p50: number | null; p95: number | null; max: number | null } | null): number {
    if (!baseline || baseline.p95 == null || baseline.p95 <= 0) return 0;
    const ceiling = baseline.p95;
    return Math.min(1, Math.max(0, valueMs / ceiling));
  }

  function p50Ratio(baseline: { p50: number | null; p95: number | null; max: number | null } | null): number | null {
    if (!baseline || baseline.p50 == null || baseline.p95 == null || baseline.p95 <= 0) return null;
    return Math.min(1, Math.max(0, baseline.p50 / baseline.p95));
  }

  /**
   * One watch per semantic scope. `load()` must not call this on a refresh, or
   * every data read would replace the subscription.
   */
  function ensureJobsWatch(id: string) {
    if (false) return;
    if (watchedJobId === id && watchController !== undefined) return;
    stopJobsWatch();
    watchedJobId = id;
    const controller = new AbortController();
    watchController = controller;

    void (async () => {
      try {
        const stream = await trellis.jobsWatch(
          { includeInitial: false, jobId: id },
          { signal: controller.signal },
        ).orThrow();
        if (controller.signal.aborted || disposed) return;
        watchLive = true;
        for await (const _event of stream) {
          if (controller.signal.aborted || disposed) return;
          watchScheduler.notify();
        }
        // A completed stream means the watch is no longer established; the
        // page keeps its last snapshot and manual refresh stays available.
        watchLive = false;
        if (watchController === controller) watchController = undefined;
      } catch {
        if (!controller.signal.aborted) {
          watchLive = false;
          if (watchController === controller) watchController = undefined;
        }
      }
    })();
  }

  onMount(() => {
    loadCurrentJobIfNeeded();
  });

  afterNavigate(() => {
    loadCurrentJobIfNeeded();
  });

  onDestroy(() => {
    disposed = true;
    ++loadSequence;
    ++metricsSequence;
    stopJobsWatch();
    watchScheduler.dispose();
    if (copyFlashTimer) clearTimeout(copyFlashTimer);
  });
</script>

<section class="job-detail">
  {#if job}
    <PageToolbar title={canShowJobName(job) ? job.type : "Job"} description={job.trigger?.kind ? `via ${job.trigger.kind}` : undefined}>
      {#snippet eyebrowExtra()}
        {#if jobDeploymentId}
          <a class="job-deployment-link break-anywhere" href={resolve(`/admin/services/${encodeURIComponent(jobDeploymentId)}`)} title={jobDeploymentId}>
            {jobDeploymentId}
          </a>
        {/if}
      {/snippet}
      {#snippet actions()}
        <div class="flex flex-wrap items-center gap-2">
          {#if canRetry}
            <button class="btn btn-primary btn-sm" onclick={() => runAction("retry")} disabled={actionBusy !== null}>
              {actionBusy === "retry" ? "Retrying…" : "Retry"}
            </button>
          {/if}
          {#if canCancel}
            <button class="btn btn-outline btn-sm" onclick={() => void guardedAction("cancel")} disabled={actionBusy !== null}>
              {actionBusy === "cancel" ? "Cancelling…" : "Cancel"}
            </button>
          {/if}
          {#if canDlq}
            <button class="btn btn-outline btn-sm" title="Requeue this dead-letter (DLQ) job for another attempt" onclick={() => runAction("replay")} disabled={actionBusy !== null}>
              {actionBusy === "replay" ? "Replaying…" : "Replay DLQ"}
            </button>
            <button class="btn btn-error btn-outline btn-sm" title="Discard this dead-letter (DLQ) job permanently" onclick={() => void guardedAction("dismiss")} disabled={actionBusy !== null}>
              {actionBusy === "dismiss" ? "Dismissing…" : "Dismiss DLQ"}
            </button>
          {/if}
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/jobs")}>Back</a>
          {#if watchLive}
            <span class="badge badge-success badge-sm" title="A live job watch is established">Live</span>
          {:else}
            <span class="badge badge-neutral badge-sm" title="No live watch; use Refresh">Not live</span>
          {/if}
          <button class="btn btn-ghost btn-sm" onclick={() => load()} disabled={loading || actionBusy !== null}>Refresh</button>
        </div>
      {/snippet}
    </PageToolbar>
  {/if}

  {#if error}
    <Notice variant="error" role="alert">{error}</Notice>
  {:else if unavailableMessage}
    <Notice variant="info" role="status">{unavailableMessage}</Notice>
  {/if}

  {#if loading}
    <LoadingState label="Loading job" />
  {:else if unavailableMessage}
    <p class="text-xs text-base-content/60">The console can still be used normally without jobs installed.</p>
  {:else if !job}
    <EmptyState title="Job not found" description="No job exists for this id." />
  {:else}
    <div class="stats-strip">
      <div class="stats-cell stats-cell-status">
        <span class="stats-cell-label">Status</span>
        <span class={["stats-cell-value tabular-nums font-semibold", job.state === "failed" || job.state === "dead" ? "status-failed" : job.state === "completed" ? "status-completed" : job.state === "active" || job.state === "retry" ? "status-active" : ""]}>
          {job.state}
        </span>
      </div>
      {#if activeWaits.length > 0}
        {@const firstWait = activeWaits[0]}
        <div class="stats-cell stats-cell-waiting" title="Current active wait edges">
          <span class="stats-cell-label">Waiting on</span>
          <span class="stats-cell-value">
            <span class="badge badge-warning badge-sm">{activeWaits.length}</span>
            <span class="trellis-identifier break-anywhere">{firstWait ? waitTargetLabel(firstWait) : UNSET}</span>
          </span>
          <span class="stats-cell-baseline tabular-nums">{firstWait ? waitDurationLabel(firstWait) : UNSET}{activeWaits.length > 1 ? ` · +${activeWaits.length - 1} more` : ""}</span>
        </div>
      {/if}
      <div class="stats-cell" title={jobRuntimeMs != null && runtimeBaseline ? `Job runtime vs p50 ${compactDuration(runtimeBaseline.p50 ?? 0)} / p95 ${compactDuration(runtimeBaseline.p95 ?? 0)} over the last ${runtimeBaseline.count} ${runtimeBaseline.count === 1n ? "job" : "jobs"} of type ${job.type}` : "Runtime since the job started"}>
        <span class="stats-cell-label">Runtime</span>
        <span class="stats-cell-value tabular-nums">{jobRuntime}</span>
        {#if runtimeBaseline}
          {@const tone = jobRuntimeMs != null ? runtimeTone(jobRuntimeMs, runtimeBaseline.p50, runtimeBaseline.p95) : "fast"}
          {@const pct = jobRuntimeMs != null ? ratio(jobRuntimeMs, runtimeBaseline) : null}
          {@const p50Pct = p50Ratio(runtimeBaseline)}
          <div class={["status-row-bar", `status-row-bar-${tone}`]} aria-hidden="true">
            {#if pct != null}
              <span class="status-row-tick" style="--bar-pct: {Math.round(pct * 100)}%"></span>
            {/if}
            {#if p50Pct != null}
              <span class="status-row-rule" style="--bar-pct: {Math.round(p50Pct * 100)}%"></span>
            {/if}
          </div>
          <span class="stats-cell-baseline tabular-nums">
            {#if runtimeBaseline.p50 != null && runtimeBaseline.p95 != null}
              p50 {compactDuration(runtimeBaseline.p50)} · p95 {compactDuration(runtimeBaseline.p95)}
            {:else}
              n {runtimeBaseline.count} job{runtimeBaseline.count === 1n ? "" : "s"}
            {/if}
          </span>
        {:else if metricsUnavailable}
          <span class="stats-cell-baseline">Baseline unavailable</span>
        {:else if metrics}
          <span class="stats-cell-baseline">No baseline yet</span>
        {:else}
          <span class="stats-cell-baseline">Loading baseline…</span>
        {/if}
      </div>
      <div class="stats-cell" title={jobQueueWaitMs != null && queueWaitBaseline ? `Queue wait vs p50 ${compactDuration(queueWaitBaseline.p50 ?? 0)} / p95 ${compactDuration(queueWaitBaseline.p95 ?? 0)} over the last ${queueWaitBaseline.count} ${queueWaitBaseline.count === 1n ? "job" : "jobs"} of type ${job.type}` : "Time the job spent in the queue before starting"}>
        <span class="stats-cell-label">Queue wait</span>
        <span class="stats-cell-value tabular-nums">{jobQueueWaitMs != null ? compactDuration(jobQueueWaitMs) : UNSET}</span>
        {#if queueWaitBaseline && jobQueueWaitMs != null}
          {@const tone = runtimeTone(jobQueueWaitMs, queueWaitBaseline.p50, queueWaitBaseline.p95)}
          {@const pct = ratio(jobQueueWaitMs, queueWaitBaseline)}
          {@const p50Pct = p50Ratio(queueWaitBaseline)}
          <div class={["status-row-bar", `status-row-bar-${tone}`]} aria-hidden="true">
            <span class="status-row-tick" style="--bar-pct: {Math.round(pct * 100)}%"></span>
            {#if p50Pct != null}
              <span class="status-row-rule" style="--bar-pct: {Math.round(p50Pct * 100)}%"></span>
            {/if}
          </div>
          <span class="stats-cell-baseline tabular-nums">
            {#if queueWaitBaseline.p50 != null && queueWaitBaseline.p95 != null}
              p50 {compactDuration(queueWaitBaseline.p50)} · p95 {compactDuration(queueWaitBaseline.p95)}
            {:else}
              n {queueWaitBaseline.count} job{queueWaitBaseline.count === 1n ? "" : "s"}
            {/if}
          </span>
        {:else if jobQueueWaitMs == null}
          <span class="stats-cell-baseline">Job not started</span>
        {/if}
      </div>
      <div class="stats-cell">
        <span class="stats-cell-label">Tries</span>
        <span class={["stats-cell-value tabular-nums", job.tries > 1n ? "text-warning" : ""]}>{job.tries}/{job.maxTries}</span>
        {#if job.tries > 1n}
          <span class="stats-cell-baseline text-warning">retrying</span>
        {:else}
          <span class="stats-cell-baseline">first attempt</span>
        {/if}
      </div>
      {#if job.deadline}
        {@const deadlinePast = new Date(job.deadline) < new Date()}
        <div class="stats-cell">
          <span class="stats-cell-label">Deadline</span>
          <span class={["stats-cell-value tabular-nums", deadlinePast ? "text-error" : ""]}>{formatDate(job.deadline)}</span>
          <span class={["stats-cell-baseline", deadlinePast ? "text-error" : ""]}>{deadlinePast ? "overdue" : "wall clock"}</span>
        </div>
      {/if}
      <div class="stats-cell">
        <span class="stats-cell-label">Created</span>
        <span class="stats-cell-value tabular-nums">{formatDate(job.createdAt)}</span>
        <span class="stats-cell-baseline">submitted</span>
      </div>
      {#if job.startedAt}
        <div class="stats-cell">
          <span class="stats-cell-label">Started</span>
          <span class="stats-cell-value tabular-nums">{formatDate(job.startedAt)}</span>
          <span class="stats-cell-baseline">worker pickup</span>
        </div>
      {/if}
      {#if job.completedAt}
        <div class="stats-cell">
          <span class="stats-cell-label">Completed</span>
          <span class="stats-cell-value tabular-nums">{formatDate(job.completedAt)}</span>
          <span class="stats-cell-baseline">terminal</span>
        </div>
      {/if}
      {#if job.errorDetail?.worker?.service}
        <div class="stats-cell" title="The worker instance that last failed this job">
          <span class="stats-cell-label">Last worker</span>
          <span class="stats-cell-value">
            <span class="trellis-identifier">{job.errorDetail.worker.service}</span>
            {#if job.errorDetail.worker.runtime}
              <span class="stats-cell-baseline">{job.errorDetail.worker.runtime}{job.errorDetail.worker.version ? ` · ${job.errorDetail.worker.version}` : ""}</span>
            {/if}
          </span>
          <span class="stats-cell-baseline">{job.errorDetail.worker.instanceId ?? "no instance"}</span>
        </div>
      {/if}
      {#if job.progress && !["completed", "failed", "dead", "cancelled", "dismissed", "expired", "skipped"].includes(job.state) && (job.progress.current !== undefined || job.progress.step !== undefined)}
        <div class="stats-cell">
          <span class="stats-cell-label">Progress</span>
          <span class="stats-cell-value tabular-nums">
            {#if job.progress.current !== undefined && job.progress.total !== undefined}
              {job.progress.current}/{job.progress.total}
            {:else if job.progress.current !== undefined}
              {job.progress.current}
            {:else}
              {job.progress.step ?? UNSET}
            {/if}
          </span>
          <span class="stats-cell-baseline">{job.progress.message ?? job.progress.step ?? "in flight"}</span>
        </div>
      {/if}
      {#if job.concurrency?.key}
        <div class="stats-cell">
          <span class="stats-cell-label">Concurrency</span>
          <span class="trellis-identifier stats-cell-value break-anywhere">{job.concurrency.key}</span>
          {#if job.concurrency.staleTakeoverCount !== undefined && job.concurrency.staleTakeoverCount > 0n}
            <span class="stats-cell-baseline text-warning">{job.concurrency.staleTakeoverCount} stale takeover{job.concurrency.staleTakeoverCount === 1n ? "" : "s"}</span>
          {:else}
            <span class="stats-cell-baseline">keyed</span>
          {/if}
        </div>
      {/if}
    </div>

    <div class="job-body">
      <div class="job-body-left">
        <div class="job-io-grid">
          {#if showOutput}
            <Panel title="Output">
              {#snippet actions()}
                {#if outputIsError}
                  <span class="badge badge-error badge-sm" aria-label="{errors.length} errors">{errors.length}</span>
                {:else}
                  <button
                    type="button"
                    class="btn btn-ghost btn-xs"
                    aria-label="Copy result"
                    disabled={job.result === undefined || job.result === null}
                    onclick={() => void copyText("job-result", job.result)}
                  >
                    {copyFlash === "job-result" ? "Copied" : "Copy"}
                  </button>
                {/if}
              {/snippet}
              {#if outputIsError}
                <ul class="errors-list">
                  {#each errors as err (err.fingerprint)}
                    {@const payload = parseErrorPayload(err.message)}
                    {@const ctx = payload && payload.context && typeof payload.context === "object" ? (payload.context as Record<string, unknown>) : null}
                    <li>
                      <div class="error-payload">
                        <div class="error-payload-summary">
                          <div class="error-payload-type">{String(payload?.type ?? "Error")}</div>
                          <div class="error-payload-message">{String(payload?.message ?? err.message)}</div>
                          {#if ctx && ctx.causeMessage}
                            <div class="error-payload-cause">{String(ctx.causeMessage)}</div>
                          {/if}
                        </div>
                        <dl class="error-context-list">
                          {#if ctx && ctx.traceId}
                            <div class="error-context-row">
                              <dt>Trace</dt>
                              <dd class="trellis-identifier break-anywhere">{String(ctx.traceId)}</dd>
                            </div>
                          {/if}
                        </dl>
                        {#if err.stack}
                          <details class="error-payload-details">
                            <summary>
                              <span>Show stack</span>
                              <button
                                type="button"
                                class="error-payload-copy"
                                aria-label="Copy stack trace"
                                onclick={(event) => {
                                  event.preventDefault();
                                  event.stopPropagation();
                                  void copyText(`error-stack-${err.fingerprint}`, err.stack ?? "");
                                }}
                              >
                                <Icon name="clipboard" size={12} />
                                <span>{copyFlash === `error-stack-${err.fingerprint}` ? "Copied" : "Copy"}</span>
                              </button>
                            </summary>
                            <pre class="error-stack" aria-label="Stack trace">{err.stack}</pre>
                          </details>
                        {/if}
                        {#if payload}
                          <details class="error-payload-details">
                            <summary>
                              <span>Show raw JSON</span>
                            </summary>
                            <JsonTree value={payload} initiallyExpanded={true} maxDepth={4} />
                          </details>
                        {/if}
                      </div>
                    </li>
                  {/each}
                </ul>
              {:else if job.result !== undefined && job.result !== null}
                <JsonTree value={job.result} initiallyExpanded={true} maxDepth={4} />
              {:else}
                <p class="text-xs text-base-content/60">No result recorded.</p>
              {/if}
            </Panel>
          {/if}

          <Panel title="Inputs">
            {#snippet actions()}
              <button
                type="button"
                class="btn btn-ghost btn-xs"
                aria-label="Copy payload"
                disabled={job.payload === undefined || job.payload === null}
                onclick={() => void copyText("job-payload", job.payload)}
              >
                {copyFlash === "job-payload" ? "Copied" : "Copy"}
              </button>
            {/snippet}
            {#if job.payload !== undefined && job.payload !== null}
              <JsonTree value={job.payload} initiallyExpanded={true} maxDepth={4} />
            {:else}
              <p class="text-xs text-base-content/60">No payload recorded.</p>
            {/if}
          </Panel>
        </div>

        <Panel title="Identity">
          <dl class="identity-list">
            <div class="identity-row">
              <dt>Job id</dt>
              <dd class="flex flex-wrap items-center gap-2">
                <span class="trellis-identifier break-anywhere">{job.id}</span>
                <button
                  type="button"
                  class="identity-copy"
                  aria-label="Copy job id"
                  onclick={() => void copyText("identity-job-id", job.id)}
                >
                  <Icon name="clipboard" size={10} />
                  <span>{copyFlash === "identity-job-id" ? "Copied" : "Copy"}</span>
                </button>
              </dd>
            </div>
            {#if job.context?.requestId}
              <div class="identity-row">
                <dt>Request id</dt>
                <dd class="flex flex-wrap items-center gap-2">
                  <span class="trellis-identifier break-anywhere">{job.context.requestId}</span>
                  <button
                    type="button"
                    class="identity-copy"
                    aria-label="Copy request id"
                    onclick={() => void copyText("identity-request-id", job.context.requestId)}
                  >
                    <Icon name="clipboard" size={10} />
                    <span>{copyFlash === "identity-request-id" ? "Copied" : "Copy"}</span>
                  </button>
                </dd>
              </div>
            {/if}
            {#if job.context?.traceId}
              <div class="identity-row">
                <dt>Trace id</dt>
                <dd class="flex flex-wrap items-center gap-2">
                  <span class="trellis-identifier break-anywhere">{job.context.traceId}</span>
                  <button
                    type="button"
                    class="identity-copy"
                    aria-label="Copy trace id"
                    onclick={() => void copyText("identity-trace-id", job.context.traceId)}
                  >
                    <Icon name="clipboard" size={10} />
                    <span>{copyFlash === "identity-trace-id" ? "Copied" : "Copy"}</span>
                  </button>
                </dd>
              </div>
            {/if}
            <div class="identity-row">
              <dt>Trigger</dt>
              <dd class="trellis-identifier break-anywhere">{job.trigger?.id ?? UNSET}</dd>
            </div>
            <div class="identity-row">
              <dt>Concurrency</dt>
              <dd class="trellis-identifier break-anywhere">{job.concurrency?.key ?? "unkeyed"}</dd>
            </div>
            <div class="identity-row">
              <dt>Created</dt>
              <dd class="tabular-nums">{formatDate(job.createdAt)}</dd>
            </div>
            <div class="identity-row">
              <dt>Updated</dt>
              <dd class="tabular-nums">{formatDate(job.updatedAt)}</dd>
            </div>
            {#if job.startedAt}
              <div class="identity-row">
                <dt>Started</dt>
                <dd class="tabular-nums">{formatDate(job.startedAt)}</dd>
              </div>
            {/if}
            {#if job.completedAt}
              <div class="identity-row">
                <dt>Completed</dt>
                <dd class="tabular-nums">{formatDate(job.completedAt)}</dd>
              </div>
            {/if}
            {#if job.deadline}
              <div class="identity-row">
                <dt>Deadline</dt>
                <dd class="tabular-nums">{formatDate(job.deadline)}</dd>
              </div>
            {/if}
            {#if job.concurrency?.staleTakeoverCount !== undefined && job.concurrency.staleTakeoverCount > 0}
              <div class="identity-row">
                <dt>Stale takeovers</dt>
                <dd class="text-warning tabular-nums">{job.concurrency.staleTakeoverCount}</dd>
              </div>
            {/if}
          </dl>
        </Panel>

        {#if activeWaits.length > 0}
          <Panel title="Current waits">
            {#snippet actions()}
              <span class="badge badge-warning badge-sm" aria-label="{activeWaits.length} active waits">{activeWaits.length}</span>
            {/snippet}
            <ul class="wait-list">
              {#each activeWaits as edge (edge.id)}
                <li class="wait-item">
                  <div class="wait-item-head">
                    <span class="badge badge-warning badge-sm">{edge.target.kind}</span>
                    <span class="wait-title">{waitTargetLabel(edge)}</span>
                  </div>
                  <dl class="wait-meta">
                    <div>
                      <dt>Kind</dt>
                      <dd>{edge.target.kind}</dd>
                    </div>
                    <div>
                      <dt>Key</dt>
                      <dd>
                        {#if edge.target.kind === "job" && edge.target.id}
                          <a class="trellis-identifier link link-hover break-anywhere" href={resolve(`/admin/jobs/${encodeURIComponent(edge.target.id)}`)}>{edge.target.id}</a>
                        {:else}
                          <span class="trellis-identifier break-anywhere">{edge.target.key ?? edge.target.operationId ?? edge.target.id ?? UNSET}</span>
                        {/if}
                      </dd>
                    </div>
                    <div>
                      <dt>Label</dt>
                      <dd>{edge.target.label ?? edge.label ?? UNSET}</dd>
                    </div>
                    <div>
                      <dt>System</dt>
                      <dd class="trellis-identifier break-anywhere">{edge.target.system ?? edge.target.service ?? UNSET}</dd>
                    </div>
                    <div>
                      <dt>Operation</dt>
                      <dd class="trellis-identifier break-anywhere">{edge.target.operation ?? edge.target.type ?? UNSET}</dd>
                    </div>
                    <div>
                      <dt>Since</dt>
                      <dd class="tabular-nums">{formatDate(edge.startedAt)}</dd>
                    </div>
                    <div>
                      <dt>Duration</dt>
                      <dd class="tabular-nums">{waitDurationLabel(edge)}</dd>
                    </div>
                  </dl>
                </li>
              {/each}
            </ul>
          </Panel>
        {/if}

        {#if (job.lineage?.rootJobId || job.lineage?.parentJobId || job.lineage?.operationId)}
          <Panel title="Where this job sits">
            <ol class="lineage-tree">
              {#if job.lineage.rootJobId}
                <li class="lineage-node">
                  <span class="lineage-label">Root</span>
                  <a class="trellis-identifier link link-hover break-anywhere" href={resolve(`/admin/jobs/${encodeURIComponent(job.lineage.rootJobId)}`)}>{job.lineage.rootJobId}</a>
                </li>
              {/if}
              {#if job.lineage.parentJobId}
                <li class="lineage-node">
                  <span class="lineage-label">Parent</span>
                  <a class="trellis-identifier link link-hover break-anywhere" href={resolve(`/admin/jobs/${encodeURIComponent(job.lineage.parentJobId)}`)}>{job.lineage.parentJobId}</a>
                </li>
              {/if}
              <li class="lineage-node lineage-node-current">
                <span class="lineage-label">Current</span>
                <span class="trellis-identifier break-anywhere">{job.id}</span>
              </li>
              {#if job.lineage.operationId}
                <li class="lineage-node">
                  <span class="lineage-label">Operation</span>
                  <span class="trellis-identifier break-anywhere">{job.lineage.operationId}</span>
                </li>
              {/if}
            </ol>
          </Panel>
        {/if}

        {#if meaningfulRelated.length > 0}
          <Panel title="Related jobs">
            {#snippet actions()}
              <span class="badge badge-ghost badge-sm" aria-label="{meaningfulRelated.length} related jobs">{meaningfulRelated.length}</span>
            {/snippet}
            <p class="related-help">Other jobs that share a trace, parent, root, operation, concurrency key, or wait edge with this one.</p>
            <ul class="related-list">
              {#each meaningfulRelated as entry (entry.job.id)}
                <li>
                  <a class="trellis-identifier link link-hover break-anywhere" href={resolve(`/admin/jobs/${encodeURIComponent(entry.job.id)}`)}>{entry.job.id}</a>
                  <StatusBadge label={entry.job.state} status={stateStatus(entry.job.state)} />
                  <span class="text-xs text-base-content/60">{entry.job.type}</span>
                  <span class="related-reason" title={`Why this job is related: ${entry.match?.label ?? ""}`}>
                    {entry.match?.label ?? ""}
                  </span>
                </li>
              {/each}
             </ul>
           </Panel>
         {/if}
       </div>

       <div class="job-body-right">
         {#if sortedAttempts.length > 0}
           <div class="attempt-tabs" role="tablist" aria-label="Attempts">
             {#each sortedAttempts as attempt, idx (`${attempt.try}`)}
               <button
                 type="button"
                 role="tab"
                 id={`attempt-tab-${idx}`}
                 aria-selected={idx === activeAttemptIndex}
                 aria-controls="attempt-summary"
                 class={["attempt-tab", idx === activeAttemptIndex ? "attempt-tab-active" : ""]}
                 onclick={() => (activeAttemptIndex = idx)}
               >
                 <span class="attempt-tab-label">#{attempt.try ?? idx + 1}</span>
                 <StatusBadge label={attempt.state ?? UNSET} status={stateStatus(attempt.state ?? "")} />
                 <span class="attempt-tab-duration">{attemptDurationLabel(attempt)}</span>
               </button>
             {/each}
           </div>
         {/if}

         <div id="attempt-summary" role="tabpanel" aria-labelledby={sortedAttempts.length > 0 ? `attempt-tab-${Math.min(activeAttemptIndex, Math.max(0, sortedAttempts.length - 1))}` : undefined}>
           <Panel title="Timeline">
             {#snippet actions()}
               <span class="badge badge-ghost badge-sm">{selectedTimeline.length} events</span>
             {/snippet}
             <JobEventTimeline events={selectedTimeline} />
           </Panel>
         </div>

         {#if job.logs && job.logs.length > 0}
           <Panel title="Logs">
             {#snippet actions()}
               <span class="badge badge-ghost badge-sm">{job.logs?.length ?? 0} entries</span>
             {/snippet}
             <DataTable size="xs">
               <thead><tr><th>Time</th><th>Level</th><th>Message</th></tr></thead>
               <tbody>
                 {#each job.logs as log (`${log.timestamp}:${log.message}`)}
                   <tr>
                     <td class="text-xs text-base-content/60">{formatDate(log.timestamp)}</td>
                     <td>
                       <span class={["badge badge-xs", log.level === "error" ? "badge-error" : log.level === "warn" ? "badge-warning" : "badge-ghost"]}>
                         {log.level}
                       </span>
                     </td>
                     <td class="text-sm">{log.message}</td>
                   </tr>
                 {/each}
               </tbody>
             </DataTable>
           </Panel>
         {/if}
       </div>
     </div>
   {/if}
 </section>

<ConfirmationModal bind:this={confirmationModal} />

<style>
  .job-detail {
    display: grid;
    gap: 0.85rem;
  }

  .job-deployment-link {
    color: color-mix(in oklab, var(--color-base-content) 62%, transparent);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 0.7rem;
    font-weight: 400;
    text-transform: none;
    letter-spacing: 0;
    text-decoration: none;
    border-bottom: 1px dotted color-mix(in oklab, var(--color-base-content) 25%, transparent);
  }

  .job-deployment-link:hover {
    color: var(--color-base-content);
    border-bottom-color: var(--color-base-content);
  }

  .job-body {
    display: grid;
    gap: 0.85rem;
  }

  .job-body-left,
  .job-body-right {
    display: grid;
    gap: 0.85rem;
    align-content: start;
  }

  .job-io-grid {
    display: grid;
    gap: 0.85rem;
  }

  @media (min-width: 900px) {
    .job-io-grid {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
  }

  @media (min-width: 1100px) {
    .job-body {
      grid-template-columns: minmax(0, 2.2fr) minmax(16rem, 1fr);
      gap: 1rem;
    }
  }

  .stats-strip {
    background: var(--color-base-100);
    border-radius: 0.75rem;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
    gap: 0.65rem 1rem;
    margin-bottom: 0.85rem;
    padding: 0.75rem 1rem;
  }

  .stats-cell {
    align-items: flex-start;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    min-width: 0;
    padding: 0.35rem 0.5rem;
    margin: -0.35rem -0.5rem;
    border-radius: 0.5rem;
    transition: background 0.2s ease-out, box-shadow 0.2s ease-out, transform 0.15s ease-out;
  }

  .stats-cell:hover {
    background: color-mix(in oklab, var(--color-base-content) 4%, transparent);
    box-shadow: 0 1px 3px color-mix(in oklab, var(--color-base-content) 8%, transparent);
    transform: translateY(-1px);
  }

  .stats-cell:active {
    transform: translateY(0);
    box-shadow: none;
  }

  .stats-cell-status {
    transition: background 0.3s ease-out, box-shadow 0.2s ease-out, transform 0.15s ease-out;
  }

  .stats-cell-status .stats-cell-value {
    font-size: 1.6rem;
    font-weight: 800;
    letter-spacing: -0.02em;
    line-height: 1.1;
  }

  .stats-cell:has(.status-row-bar) .stats-cell-value {
    font-size: 1rem;
    font-weight: 700;
  }

  @keyframes deadline-pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.7; }
  }

  @media (prefers-reduced-motion: no-preference) {
    .stats-cell:has(.text-error) .stats-cell-value {
      animation: deadline-pulse 2s ease-in-out infinite;
    }
  }

  @keyframes retry-emphasis {
    0%, 100% { transform: scale(1); }
    50% { transform: scale(1.02); }
  }

  @media (prefers-reduced-motion: no-preference) {
    .stats-cell:has(.text-warning) .stats-cell-value {
      animation: retry-emphasis 1.5s ease-in-out infinite;
    }
  }

  .stats-cell-label {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.68rem;
    font-weight: 500;
    letter-spacing: 0.05em;
    text-transform: uppercase;
  }

  .stats-cell-value {
    align-items: baseline;
    color: var(--color-base-content);
    display: flex;
    flex-wrap: wrap;
    font-size: 0.9rem;
    font-weight: 600;
    gap: 0.35rem;
  }

  .status-failed {
    color: oklch(0.55 0.15 25);
  }

  .status-completed {
    color: oklch(0.55 0.15 145);
  }

  .status-active {
    color: oklch(0.55 0.15 85);
  }

  .stats-cell-waiting {
    background: color-mix(in oklab, var(--color-warning) 9%, transparent);
  }

  .stats-cell-baseline {
    color: color-mix(in oklab, var(--color-base-content) 62%, transparent);
    font-size: 0.68rem;
    font-weight: 400;
    letter-spacing: 0.02em;
  }

  .status-row-bar {
    background: color-mix(in oklab, var(--color-base-300) 60%, transparent);
    border-radius: 999px;
    height: 6px;
    margin-top: 0.2rem;
    position: relative;
    width: 100%;
    overflow: hidden;
  }

  .status-row-bar-fast {
    background: color-mix(in oklab, var(--color-success) 40%, var(--color-base-300) 40%);
  }

  .status-row-bar-normal {
    background: color-mix(in oklab, var(--color-warning) 40%, var(--color-base-300) 40%);
  }

  .status-row-bar-slow {
    background: color-mix(in oklab, oklch(0.65 0.18 55) 50%, var(--color-base-300) 30%);
  }

  .status-row-tick {
    background: var(--color-base-content);
    border-radius: 999px;
    height: 10px;
    left: var(--bar-pct, 0%);
    position: absolute;
    top: 50%;
    transform: translate(-50%, -50%);
    width: 2px;
  }

  .status-row-rule {
    background: color-mix(in oklab, var(--color-base-content) 30%, transparent);
    height: 6px;
    left: var(--bar-pct, 0%);
    position: absolute;
    top: 50%;
    transform: translate(-50%, -50%);
    width: 1px;
  }

  .status-row-baseline-text {
    color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
    font-size: 0.65rem;
    letter-spacing: 0.02em;
  }

  .identity-copy {
    align-items: center;
    background: transparent;
    border: 1px solid color-mix(in oklab, var(--color-base-300) 50%, transparent);
    border-radius: 0.3rem;
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    cursor: pointer;
    display: inline-flex;
    font-size: 0.65rem;
    gap: 0.3rem;
    letter-spacing: 0.04em;
    padding: 0.05rem 0.4rem;
    text-transform: uppercase;
  }

  .identity-copy:hover {
    background: color-mix(in oklab, var(--color-base-300) 25%, transparent);
    color: var(--color-base-content);
  }

  .identity-copy:focus-visible {
    box-shadow: var(--trellis-focus-ring);
    outline: none;
  }

  .attempt-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0.35rem;
  }

  .attempt-tab {
    align-items: baseline;
    background: transparent;
    border: 1px solid color-mix(in oklab, var(--color-base-300) 55%, transparent);
    border-radius: 0.35rem;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    cursor: pointer;
    display: inline-flex;
    font-size: 0.7rem;
    gap: 0.4rem;
    padding: 0.25rem 0.55rem;
  }

  .attempt-tab:hover {
    background: color-mix(in oklab, var(--color-base-300) 25%, transparent);
    color: var(--color-base-content);
  }

  .attempt-tab:focus-visible {
    box-shadow: var(--trellis-focus-ring);
    outline: none;
  }

  .attempt-tab-active {
    background: var(--color-base-content);
    border-color: var(--color-base-content);
    color: var(--color-base-100);
  }

  .attempt-tab-number {
    font-weight: 600;
  }

  .attempt-tab-state {
    font-size: 0.65rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .lineage-tree {
    display: grid;
    gap: 0.5rem;
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .lineage-node {
    align-items: center;
    border-left: 2px solid color-mix(in oklab, var(--color-base-300) 50%, transparent);
    display: flex;
    gap: 0.65rem;
    padding: 0.3rem 0 0.3rem 0.85rem;
  }

  .lineage-node-current {
    border-left-color: var(--color-accent);
  }

  .lineage-label {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.68rem;
    letter-spacing: 0.04em;
    min-width: 4.5rem;
    text-transform: uppercase;
  }

  .wait-list {
    display: grid;
    gap: 0.45rem;
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .wait-item {
    border-bottom: 1px solid color-mix(in oklab, var(--color-base-300) 45%, transparent);
    display: grid;
    gap: 0.45rem;
    padding: 0.35rem 0 0.5rem;
  }

  .wait-item:last-child {
    border-bottom: none;
  }

  .wait-item-head {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 0.45rem;
  }

  .wait-title {
    color: var(--color-base-content);
    font-size: 0.82rem;
    font-weight: 600;
  }

  .wait-meta {
    display: grid;
    gap: 0.25rem 0.7rem;
    grid-template-columns: repeat(auto-fit, minmax(8rem, 1fr));
    margin: 0;
  }

  .wait-meta div {
    min-width: 0;
  }

  .wait-meta dt {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.68rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .wait-meta dd {
    color: var(--color-base-content);
    font-size: 0.74rem;
    margin: 0;
  }

  .related-help {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.75rem;
    line-height: 1.4;
    margin: 0 0 0.4rem;
  }

  .related-list {
    display: grid;
    gap: 0.4rem;
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .related-list li {
    align-items: center;
    border-bottom: 1px solid color-mix(in oklab, var(--color-base-300) 40%, transparent);
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    padding: 0.3rem 0;
  }

  .related-list li:last-child {
    border-bottom: none;
  }

  .related-reason {
    background: color-mix(in oklab, var(--color-base-200) 60%, transparent);
    border-radius: 0.3rem;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    font-size: 0.65rem;
    letter-spacing: 0.04em;
    margin-left: auto;
    padding: 0.05rem 0.4rem;
    text-transform: uppercase;
  }

  :global(.error-payload) {
    display: grid;
    gap: 0.5rem;
    min-width: 0;
  }

  :global(.error-payload-summary) {
    background: color-mix(in oklab, var(--color-error) 8%, transparent);
    border-radius: 0.5rem;
    padding: 0.7rem 0.85rem;
    display: grid;
    gap: 0.25rem;
  }

  :global(.error-payload-type) {
    color: var(--color-error);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 0.7rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  :global(.error-payload-message) {
    color: var(--color-base-content);
    font-size: 0.95rem;
    font-weight: 600;
    line-height: 1.3;
  }

  :global(.error-payload-cause) {
    color: color-mix(in oklab, var(--color-base-content) 85%, transparent);
    font-size: 0.85rem;
    line-height: 1.4;
  }

  :global(.error-context-list) {
    display: grid;
    gap: 0.2rem 0.85rem;
    font-size: 0.72rem;
    margin: 0;
    padding: 0;
  }

  :global(.error-context-row) {
    align-items: baseline;
    display: grid;
    grid-template-columns: 5.5rem minmax(0, 1fr);
    gap: 0.5rem;
  }

  :global(.error-context-row dt) {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    font-size: 0.65rem;
  }

  :global(.error-context-row dd) {
    color: var(--color-base-content);
    margin: 0;
  }

  :global(.error-payload-details) {
    border-top: 1px solid color-mix(in oklab, var(--color-base-300) 55%, transparent);
    font-size: 0.72rem;
    margin-top: 0.2rem;
    padding-top: 0.35rem;
  }

  :global(.error-payload-details summary) {
    align-items: center;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    cursor: pointer;
    display: flex;
    gap: 0.5rem;
    list-style: none;
  }

  :global(.error-payload-details summary::-webkit-details-marker) {
    display: none;
  }

  :global(.error-payload-details summary::before) {
    content: "▸ ";
  }

  :global(.error-payload-details[open] summary::before) {
    content: "▾ ";
  }

  :global(.error-payload-details summary > :first-child) {
    flex: 1;
  }

  :global(.error-payload-copy) {
    align-items: center;
    background: transparent;
    border: 1px solid color-mix(in oklab, var(--color-base-300) 70%, transparent);
    border-radius: 0.35rem;
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    cursor: pointer;
    display: inline-flex;
    font-size: 0.65rem;
    font-weight: 600;
    gap: 0.3rem;
    letter-spacing: 0.03em;
    padding: 0.15rem 0.45rem;
    text-transform: uppercase;
  }

  :global(.error-payload-copy:hover) {
    background: color-mix(in oklab, var(--color-base-300) 25%, transparent);
    color: var(--color-base-content);
  }

  :global(.error-payload-copy:focus-visible) {
    box-shadow: var(--trellis-focus-ring);
    outline: none;
  }

  :global(.error-payload-details pre) {
    margin: 0.4rem 0 0;
  }

  .errors-list {
    display: grid;
    gap: 0.75rem;
  }

  .identity-list {
    display: grid;
    gap: 0.4rem 0.85rem;
    grid-template-columns: 1fr;
    margin: 0;
  }

  @media (min-width: 640px) {
    .identity-list {
      grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    }
  }

  .identity-row {
    align-items: baseline;
    display: grid;
    grid-template-columns: 6rem minmax(0, 1fr);
    gap: 0.5rem;
  }

  .identity-list dt {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.72rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .identity-list dd {
    color: color-mix(in oklab, var(--color-base-content) 80%, transparent);
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }

  .error-stack {
    background: color-mix(in oklab, var(--color-base-200) 64%, var(--color-base-100));
    border-radius: var(--radius-field, 0.5rem);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 0.72rem;
    line-height: 1.5;
    margin: 0.4rem 0 0;
    max-height: 12rem;
    overflow: auto;
    padding: 0.6rem 0.75rem;
    white-space: pre-wrap;
    word-break: break-word;
  }

  :global(.error-stack-pretty) {
    font-size: 0.75rem;
    line-height: 1.55;
    max-height: 16rem;
    padding: 0.7rem 0.85rem;
    white-space: pre;
  }

  :global(.error-stack-tree) {
    font-size: 0.78rem;
    line-height: 1.65;
    max-height: 18rem;
    padding: 0.7rem 0.85rem 0.7rem 0;
    white-space: pre;
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
  }

  :global(.error-stack-tree) :global(.json-key) {
    color: var(--color-base-content);
  }

  :global(.error-stack-tree) :global(.json-string) {
    color: oklch(0.6 0.16 145);
  }

  :global(.error-stack-tree) :global(.json-num),
  :global(.error-stack-tree) :global(.json-bool) {
    color: oklch(0.62 0.16 245);
  }

  :global(.error-stack-tree) :global(.json-null) {
    color: oklch(0.6 0.18 25);
  }

  :global(.error-stack-wide) {
    font-size: 0.78rem;
    line-height: 1.7;
    max-height: 20rem;
    padding: 0.85rem 1rem;
    white-space: pre;
    border: 1px solid color-mix(in oklab, var(--color-error) 35%, var(--color-base-300));
  }

  .break-anywhere {
    overflow-wrap: anywhere;
    white-space: normal;
  }
</style>
