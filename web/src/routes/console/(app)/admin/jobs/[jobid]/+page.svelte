<script lang="ts">
  import { resolve } from "$lib/console_paths";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import { onDestroy, onMount } from "svelte";
  import { RefreshScheduler } from "$lib/console/live_refresh.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import JobEventTimeline from "$lib/components/JobEventTimeline.svelte";
  import { jobTimelineEventsForAttempt } from "$lib/job_event_timeline";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import {
    boundedNumber,
    errorMessage,
    formatDate,
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
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { decodeKnownJsonBytes } from "$lib/console/display_value.ts";
  import DataPanel from "$lib/components/DataPanel.svelte";
  import { errorText, friendlyDuration, jobStateLabel, sentenceName } from "$lib/console/plain_labels.ts";
  import { createServiceNames } from "$lib/console/service_names.svelte.ts";

  const trellis = getTrellis();
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
  const decodedPayload = $derived(decodeKnownJsonBytes(job?.payload));
  const payloadValue = $derived(decodedPayload.ok ? decodedPayload.value : job?.payload);
  const decodedResult = $derived(decodeKnownJsonBytes(job?.result));
  const resultValue = $derived(decodedResult.ok ? decodedResult.value : job?.result);
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

  const names = createServiceNames();
  const serviceName = $derived(job ? names.participant(job.service) : "");
  const serviceDeploymentId = $derived(job ? names.deploymentFor(job.service) : undefined);



  const relatedReasonLabels = {
    trace: "Same trace",
    parent: "Same parent",
    root: "Same root",
    operation: "Same operation",
    concurrency: "Same concurrency key",
    wait: "Wait edge",
  } as const;
  const triggerLabels: Record<string, string> = {
    schedule: "Schedule",
    operation: "Operation",
    rpc: "RPC handler",
    event: "Event handler",
    manualReplay: "Administrator replay",
    serviceCode: "Service code",
    parentJob: "Parent job",
  };

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
    return friendlyDuration(Date.now() - startedMs);
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
    return friendlyDuration(endMs - startMs);
  });

  const UNSET = "–";


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
        ? "It isn't known whether that worked. Refresh and check the job before trying again."
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
            title: "Dismiss this job?",
            message: "It won't run again and leaves the list of jobs needing attention. This can't be undone.",
            confirmLabel: "Dismiss",
          }
        : {
            title: "Cancel this job?",
            message: "It stops now. Anything it already finished isn't undone.",
            confirmLabel: "Cancel job",
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
      } else {
        metrics = null;
      }
    } catch {
      if (sequence !== metricsSequence || disposed) return;
      metrics = null;
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

  /** One sentence saying where the job stands. */
  const statusSentence = $derived.by(() => {
    if (!job) return "";
    const tries = `${job.tries} of ${job.maxTries} ${Number(job.maxTries) === 1 ? "try" : "tries"}`;
    switch (job.state) {
      case "completed": return jobRuntime === UNSET ? "Finished." : `Finished in ${jobRuntime}.`;
      case "failed": return `Failed after ${tries}.`;
      case "dead": return `Failed for good after ${tries}. Run it again or dismiss it.`;
      case "retry": return `Failed and will try again (${tries} used).`;
      case "pending": return jobQueueWaitMs === null ? "Waiting for a service to pick it up." : `Waiting to start.`;
      case "active":
        if (activeWaits.length > 0) return `Waiting for ${activeWaits.length === 1 ? waitTargetLabel(activeWaits[0]) : `${activeWaits.length} things`}.`;
        return `Running for ${jobRuntime}.`;
      case "stale": return "Stuck: the service running it stopped responding.";
      case "cancelled": return "Cancelled.";
      case "dismissed": return "Dismissed.";
      case "expired": return "Missed its deadline before it could run.";
      default: return sentenceName(job.state) + ".";
    }
  });

  function usually(baseline: { p50: number | null } | null): string | null {
    return baseline?.p50 != null ? `usually ${friendlyDuration(baseline.p50)}` : null;
  }
</script>

<svelte:head><title>{job ? sentenceName(job.type) : "Job"} · Trellis Console</title></svelte:head>

<section class="job-detail">
  {#if job}
    {@const state = jobStateLabel(job)}
    <PageToolbar title={canShowJobName(job) ? sentenceName(job.type) : "Job"}>
      {#snippet eyebrowExtra()}
        {#if serviceDeploymentId}
          <a class="link link-hover" href={resolve(`/admin/services/${encodeURIComponent(serviceDeploymentId)}`)}>{serviceName}</a>
        {:else}
          <span>{serviceName}</span>
        {/if}
      {/snippet}
      {#snippet meta()}
        <StatusBadge label={state.label} status={state.tone} />
        <span class={["badge badge-sm", watchLive ? "badge-success" : "badge-neutral"]}>{watchLive ? "Live" : "Not live"}</span>
      {/snippet}
      {#snippet actions()}
        <div class="flex flex-wrap items-center gap-2">
          {#if canRetry}
            <button class="btn btn-primary btn-sm" onclick={() => runAction("retry")} disabled={actionBusy !== null}>
              {actionBusy === "retry" ? "Starting…" : "Run again"}
            </button>
          {/if}
          {#if canDlq}
            <button class="btn btn-primary btn-sm" onclick={() => runAction("replay")} disabled={actionBusy !== null}>
              {actionBusy === "replay" ? "Starting…" : "Run again"}
            </button>
            <button class="btn btn-ghost btn-sm text-error" onclick={() => void guardedAction("dismiss")} disabled={actionBusy !== null}>
              {actionBusy === "dismiss" ? "Dismissing…" : "Dismiss"}
            </button>
          {/if}
          {#if canCancel}
            <button class="btn btn-outline btn-sm" onclick={() => void guardedAction("cancel")} disabled={actionBusy !== null}>
              {actionBusy === "cancel" ? "Cancelling…" : "Cancel"}
            </button>
          {/if}
          <button class="btn btn-ghost btn-sm" onclick={() => load()} disabled={loading || actionBusy !== null}>Refresh</button>
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/jobs")}>Back</a>
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
    <p class="text-sm text-base-content/60">Jobs can't be shown right now. The rest of the Console still works.</p>
  {:else if !job}
    <EmptyState title="Job not found" description="It may have finished and been cleaned up." />
  {:else}
    <div class="job-body">
      <div class="job-main">
        <div class="trellis-section p-5">
          <p class="text-lg font-medium">{statusSentence}</p>
          {#if errors.length > 0}
            {@const latest = errors[errors.length - 1]}
            <div class="error-box mt-3">
              <p class="font-medium">{errorText(latest.message) ?? "No reason was given."}</p>
              {#if errors.length > 1}<p class="mt-1 text-sm text-base-content/65">{errors.length} problems recorded across its tries.</p>{/if}
              {#if latest.stack}
                <details class="mt-2 text-sm">
                  <summary class="cursor-pointer text-base-content/70">Where it failed</summary>
                  <pre class="error-stack" aria-label="Stack trace">{latest.stack}</pre>
                </details>
              {/if}
            </div>
          {/if}
          {#if job.progress && job.state === "active" && (job.progress.current !== undefined || job.progress.message || job.progress.step)}
            <div class="mt-3">
              <p class="text-sm">{job.progress.message ?? job.progress.step ?? "Working"}</p>
              {#if job.progress.current !== undefined && job.progress.total !== undefined && job.progress.total > 0n}
                <div class="job-progress mt-1.5"><i style:--job-progress={`${Math.min(100, boundedNumber(job.progress.current) / boundedNumber(job.progress.total) * 100)}%`}></i></div>
                <p class="mt-1 text-xs text-base-content/60">{job.progress.current} of {job.progress.total}</p>
              {/if}
            </div>
          {/if}
          <dl class="fact-list mt-4">
            <div><dt>Created</dt><dd>{formatDate(job.createdAt)}</dd></div>
            <div>
              <dt>Waited to start</dt>
              <dd>{jobQueueWaitMs != null ? friendlyDuration(jobQueueWaitMs) : "Not started"}{#if jobQueueWaitMs != null && usually(queueWaitBaseline)}<span class="fact-note">&nbsp;· {usually(queueWaitBaseline)}</span>{/if}</dd>
            </div>
            <div>
              <dt>{job.state === "active" ? "Running for" : "Took"}</dt>
              <dd>{jobRuntime}{#if jobRuntimeMs != null && runtimeBaseline}{@const tone = runtimeTone(jobRuntimeMs, runtimeBaseline.p50, runtimeBaseline.p95)}<span class={["fact-note", tone === "slow" ? "text-warning" : ""]}>&nbsp;· {tone === "slow" ? "slower than usual" : usually(runtimeBaseline)}</span>{/if}</dd>
            </div>
            <div><dt>Tries</dt><dd>{job.tries} of {job.maxTries}</dd></div>
            {#if job.trigger}<div><dt>Started by</dt><dd>{triggerLabels[job.trigger.kind] ?? sentenceName(job.trigger.kind)}</dd></div>{/if}
            {#if job.deadline && ["pending", "active", "retry"].includes(job.state)}
              {@const deadlinePast = new Date(job.deadline) < new Date()}
              <div><dt>Must finish by</dt><dd class:text-error={deadlinePast}>{formatDate(job.deadline)}{deadlinePast ? " (missed)" : ""}</dd></div>
            {/if}
            {#if job.concurrency?.staleTakeoverCount !== undefined && job.concurrency.staleTakeoverCount > 0}
              <div><dt>Restarted after getting stuck</dt><dd class="text-warning">{job.concurrency.staleTakeoverCount} {Number(job.concurrency.staleTakeoverCount) === 1 ? "time" : "times"}</dd></div>
            {/if}
          </dl>
        </div>

        {#if activeWaits.length > 0}
          <Panel title="Waiting for">
            <ul class="plain-list">
              {#each activeWaits as edge (edge.id)}
                <li>
                  {#if edge.target.kind === "job" && edge.target.id}
                    <a class="link link-hover font-medium" href={resolve(`/admin/jobs/${encodeURIComponent(edge.target.id)}`)}>{waitTargetLabel(edge)}</a>
                  {:else}
                    <span class="font-medium">{waitTargetLabel(edge)}</span>
                  {/if}
                  <span class="text-sm text-base-content/60"> · for {waitDurationLabel(edge)}</span>
                </li>
              {/each}
            </ul>
          </Panel>
        {/if}

        <div class="job-io-grid">
          <div class="trellis-section p-5">
            {#if job.payload !== undefined && job.payload !== null}
              <DataPanel value={payloadValue} label="Input" />
            {:else}
              <h3 class="text-sm font-semibold">Input</h3>
              <p class="mt-2 text-sm text-base-content/60">No input.</p>
            {/if}
          </div>
          {#if job.result !== undefined && job.result !== null}
            <div class="trellis-section p-5">
              <DataPanel value={resultValue} label="Result" />
            </div>
          {/if}
        </div>

        {#if job.lineage?.parentJobId || job.lineage?.rootJobId || meaningfulRelated.length > 0}
          <Panel title="Related jobs">
            <ul class="plain-list">
              {#if job.lineage?.parentJobId}
                <li><a class="link link-hover" href={resolve(`/admin/jobs/${encodeURIComponent(job.lineage.parentJobId)}`)}>The job that started this one</a></li>
              {/if}
              {#if job.lineage?.rootJobId && job.lineage.rootJobId !== job.lineage.parentJobId}
                <li><a class="link link-hover" href={resolve(`/admin/jobs/${encodeURIComponent(job.lineage.rootJobId)}`)}>The first job in this chain</a></li>
              {/if}
              {#each meaningfulRelated as entry (entry.job.id)}
                {@const relatedState = jobStateLabel(entry.job)}
                <li class="flex flex-wrap items-center gap-2">
                  <a class="link link-hover" href={resolve(`/admin/jobs/${encodeURIComponent(entry.job.id)}`)}>{sentenceName(entry.job.type)}</a>
                  <StatusBadge label={relatedState.label} status={relatedState.tone} />
                  <span class="text-xs text-base-content/60">{entry.match?.label ?? ""}</span>
                </li>
              {/each}
            </ul>
          </Panel>
        {/if}

        <details class="trellis-section more-details">
          <summary>More details</summary>
          <dl class="fact-list mt-3">
            <div>
              <dt>Job ID</dt>
              <dd class="flex items-center gap-2"><span class="trellis-identifier break-anywhere">{job.id}</span><button type="button" class="identity-copy" aria-label="Copy job ID" onclick={() => void copyText("identity-job-id", job.id)}><Icon name={copyFlash === "identity-job-id" ? "check" : "clipboard"} size={12} /></button></dd>
            </div>
            {#if job.context?.traceId}
              <div>
                <dt>Trace ID</dt>
                <dd class="flex items-center gap-2"><span class="trellis-identifier break-anywhere">{job.context.traceId}</span><button type="button" class="identity-copy" aria-label="Copy trace ID" onclick={() => void copyText("identity-trace-id", job.context.traceId)}><Icon name={copyFlash === "identity-trace-id" ? "check" : "clipboard"} size={12} /></button></dd>
              </div>
            {/if}
            {#if job.concurrency?.key}
              <div><dt>Runs one at a time with</dt><dd class="trellis-identifier break-anywhere">{job.concurrency.key}</dd></div>
            {/if}
          </dl>
        </details>
      </div>

      <div class="job-side">
        {#if sortedAttempts.length > 1}
          <div class="trellis-segment" role="tablist" aria-label="Tries">
            {#each sortedAttempts as attempt, idx (`${attempt.try}`)}
              <button
                type="button"
                role="tab"
                id={`attempt-tab-${idx}`}
                aria-selected={idx === activeAttemptIndex}
                aria-controls="attempt-summary"
                class:active={idx === activeAttemptIndex}
                onclick={() => (activeAttemptIndex = idx)}
              >Try {attempt.try ?? idx + 1}</button>
            {/each}
          </div>
        {/if}
        <div id="attempt-summary" role="tabpanel" aria-labelledby={sortedAttempts.length > 1 ? `attempt-tab-${Math.min(activeAttemptIndex, Math.max(0, sortedAttempts.length - 1))}` : undefined}>
          <Panel title="What happened">
            <JobEventTimeline events={selectedTimeline} />
          </Panel>
        </div>

        {#if job.logs && job.logs.length > 0}
          <Panel title="Log">
            <ol class="log-list">
              {#each job.logs as log (`${log.timestamp}:${log.message}`)}
                <li>
                  <span class={["log-level", log.level === "error" ? "text-error" : log.level === "warn" ? "text-warning" : "text-base-content/60"]}>{log.level === "warn" ? "Warning" : sentenceName(log.level)}</span>
                  <span class="min-w-0 break-anywhere">{log.message}</span>
                  <time class="text-xs text-base-content/55" datetime={log.timestamp}>{formatDate(log.timestamp)}</time>
                </li>
              {/each}
            </ol>
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
    gap: 1.25rem;
    grid-template-columns: minmax(0, 1fr);
  }

  .job-body {
    align-items: start;
    display: grid;
    gap: 1.25rem;
    grid-template-columns: minmax(0, 1fr) minmax(18rem, 26rem);
  }

  .job-main,
  .job-side {
    display: grid;
    gap: 1.25rem;
    min-width: 0;
  }

  .error-box {
    background: color-mix(in oklab, var(--color-error) 7%, var(--color-base-100));
    border: 1px solid color-mix(in oklab, var(--color-error) 25%, var(--color-base-300));
    border-radius: var(--radius-field);
    padding: 0.8rem 1rem;
  }

  .error-stack {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 0.75rem;
    margin-top: 0.5rem;
    max-height: 18rem;
    overflow: auto;
    white-space: pre-wrap;
  }

  .job-progress {
    background: color-mix(in oklab, var(--color-base-300) 70%, transparent);
    border-radius: 999px;
    height: 0.35rem;
    overflow: hidden;
  }

  .job-progress i {
    background: var(--color-primary);
    display: block;
    height: 100%;
    width: var(--job-progress);
  }

  .fact-list {
    display: grid;
    gap: 0.75rem 1.5rem;
    grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
  }

  .fact-list dt {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.8125rem;
  }

  .fact-list dd {
    font-size: 0.9rem;
    min-width: 0;
  }

  .fact-note {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.8125rem;
  }

  .job-io-grid {
    display: grid;
    gap: 1.25rem;
    grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr));
  }

  .plain-list {
    display: grid;
    gap: 0.5rem;
  }

  .more-details {
    padding: 0.9rem 1.25rem;
  }

  .more-details summary {
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
    cursor: pointer;
    font-size: 0.875rem;
  }

  .identity-copy {
    background: transparent;
    border: 0;
    color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
    cursor: pointer;
  }

  .log-list {
    display: grid;
    gap: 0.5rem;
    font-size: 0.875rem;
  }

  .log-list li {
    display: grid;
    gap: 0.15rem 0.6rem;
    grid-template-columns: 4.5rem minmax(0, 1fr);
  }

  .log-list time {
    grid-column: 2;
  }

  .log-level {
    font-size: 0.8125rem;
  }

  .break-anywhere {
    overflow-wrap: anywhere;
  }

  @media (max-width: 1100px) {
    .job-body {
      grid-template-columns: minmax(0, 1fr);
    }
  }
</style>
