<script lang="ts">
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { onDestroy, untrack } from "svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import HealthStatus from "$lib/components/HealthStatus.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Pager from "$lib/components/Pager.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { errorMessage, formatDate, humanizeName } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { nextCursorPage, previousCursorPage } from "$lib/cursor_history.ts";
  import { TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import {
    beginOwnedWatch,
    LiveSubscription,
    RefreshScheduler,
    type WatchAttempt,
  } from "$lib/console/live_refresh.ts";

  type Participant = apis.health.QueryOutput["items"][number];

  const trellis = getTrellis();
  const RPC_TIMEOUT_MS = 10_000;

  let snapshot = $state.raw<apis.health.QueryOutput | null>(null);
  let summary = $state.raw<apis.health.SummaryOutput | null>(null);
  let inspection = $state.raw<apis.health.InspectOutput | null>(null);
  let healthMetrics = $state.raw<apis.health.MetricsOutput | null>(null);
  let loading = $state(true);
  let detailLoading = $state(false);
  let error = $state<string | null>(null);
  let watchError = $state<string | null>(null);
  let metricsError = $state<string | null>(null);
  let detailSequence = 0;
  let loadSequence = 0;
  let disposed = false;
  let watchAttempt: WatchAttempt<unknown> | null = null;
  let subscription: LiveSubscription | null = null;
  const refreshScheduler = new RefreshScheduler({
    delayMs: 250,
    canRefresh: () => !disposed && document.visibilityState === "visible",
    onRefresh: refresh,
    onRefreshError: (cause) => {
      watchError = errorMessage(cause);
    },
  });
  let selectedKey = $state<string | null>(null);
  let cursor = $state<string | undefined>();
  let cursorBackStack = $state<string[]>([]);
  let nextCursor = $state<string | undefined>();

  const participants = $derived(snapshot?.items ?? []);
  const selectedParticipant = $derived(
    participants.find(
      (participant) => participantKey(participant) === selectedKey,
    ) ??
      participants[0] ??
      null,
  );
  const instances = $derived(inspection?.instances ?? []);
  const selectedInstance = $derived(instances[0] ?? null);
  let statusFilter = $state<"all" | "attention">("all");
  // Counts describe the loaded page, so they are labelled with its scope.
  const statusCounts = $derived(
    ["healthy", "degraded", "unhealthy", "offline"].map((status) => ({
      status,
      count: participants.filter(
        (participant) => participant.effectiveStatus === status,
      ).length,
    })),
  );
  const visibleParticipants = $derived(
    statusFilter === "all"
      ? participants
      : participants.filter(
          (participant) => participant.effectiveStatus !== "healthy",
        ),
  );

  function participantKey(participant: Participant): string {
    return `${participant.participantKind}:${participant.contractId}`;
  }

  function formatKind(kind: string): string {
    return kind === "device" ? "Device" : "Service";
  }

  function onlineSummary(participant: {
    onlineInstances: bigint;
    offlineInstances: bigint;
  }): string {
    const online = Number(participant.onlineInstances);
    return `${online} of ${online + Number(participant.offlineInstances)} running`;
  }

  function formatRelativeTime(value: string, reference = Date.now()): string {
    const seconds = Math.max(
      0,
      Math.floor((reference - Date.parse(value)) / 1000),
    );
    if (seconds < 60) return `${seconds}s ago`;
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours}h ago`;
    return `${Math.floor(hours / 24)}d ago`;
  }

  function formatAvailability(value: number | null | undefined): string {
    return value == null ? "Not enough data" : `${(value * 100).toFixed(2)}%`;
  }

  const CHANGE_REASONS: Record<string, string> = {
    "first-sample": "First report",
    "heartbeat-resumed": "Started reporting again",
    "heartbeat-change": "Reported a new status",
    "deadline-expired": "Stopped reporting",
  };

  function changeReason(reason: string): string {
    return CHANGE_REASONS[reason] ?? reason;
  }

  /** Readable key/value pairs from reported info; nested values are skipped. */
  function infoEntries(info: unknown): [string, string][] {
    if (!info || typeof info !== "object") return [];
    const entries: [string, string][] = [];
    for (const [key, value] of Object.entries(info)) {
      const items = Array.isArray(value) ? value : [value];
      if (
        items.length === 0 ||
        !items.every((item) =>
          ["string", "number", "boolean", "bigint"].includes(typeof item),
        )
      )
        continue;
      entries.push([humanizeName(key), items.map(String).join(", ")]);
    }
    return entries;
  }

  async function loadParticipants(): Promise<void> {
    const sequence = ++loadSequence;
    // Query is primary; Summary is an independent panel.
    const [result, summaryResult] = await Promise.allSettled([
      trellis
        .healthQuery(
          {
            page: {
              ...(cursor === undefined ? {} : { cursor }),
              limit: TABLE_PAGE_LIMIT,
            },
          },
          { timeout: RPC_TIMEOUT_MS },
        )
        .take(),
      trellis.healthSummary({}, { timeout: RPC_TIMEOUT_MS }).take(),
    ]);
    if (sequence !== loadSequence || disposed) return;
    if (result.status === "rejected") throw result.reason;
    if (isErr(result.value)) throw result.value;
    snapshot = result.value;
    nextCursor = result.value.page.nextCursor;
    if (!selectedKey && result.value.items[0]) {
      selectedKey = participantKey(result.value.items[0]);
    }
    if (
      summaryResult.status === "fulfilled" &&
      summaryResult.value !== null &&
      !isErr(summaryResult.value)
    ) {
      summary = summaryResult.value;
    } else {
      // The summary only dates the page; the list stands without it.
      summary = null;
    }
  }

  function goPrevious() {
    const previous = { cursor, back: cursorBackStack };
    ({ cursor, back: cursorBackStack } = previousCursorPage(previous));
    selectedKey = null;
    void refresh().then(() => {
      // A failed page retains the last valid cursor and page data.
      if (error !== null) {
        cursor = previous.cursor;
        cursorBackStack = previous.back;
      }
    });
  }

  function goNext() {
    if (!nextCursor) return;
    const previous = { cursor, back: cursorBackStack };
    ({ cursor, back: cursorBackStack } = nextCursorPage(previous, nextCursor));
    selectedKey = null;
    void refresh().then(() => {
      if (error !== null) {
        cursor = previous.cursor;
        cursorBackStack = previous.back;
      }
    });
  }

  async function loadDetail(participant: Participant | null): Promise<void> {
    const sequence = ++detailSequence;
    metricsError = null;
    if (!participant) {
      inspection = null;
      healthMetrics = null;
      return;
    }
    // Capture the exact identity before awaiting so a fast selection change
    // cannot show A's metrics under B's title.
    const participantKind = participant.participantKind;
    const contractId = participant.contractId;
    detailLoading = true;
    const end = new Date();
    const start = new Date(end.getTime() - 24 * 60 * 60 * 1000);
    const [inspectResult, metricsResult] = await Promise.allSettled([
      trellis
        .healthInspect(
          { participantKind, contractId, historyLimit: 100n },
          { timeout: RPC_TIMEOUT_MS },
        )
        .take(),
      trellis
        .healthMetrics(
          {
            participantKind,
            contractId,
            start: start.toISOString(),
            end: end.toISOString(),
            stepMs: 60n * 60n * 1000n,
          },
          { timeout: RPC_TIMEOUT_MS },
        )
        .take(),
    ]);
    if (sequence !== detailSequence || disposed) return;
    try {
      if (
        inspectResult.status === "fulfilled" &&
        inspectResult.value !== null &&
        !isErr(inspectResult.value)
      ) {
        inspection = inspectResult.value;
      } else {
        inspection = null;
        error =
          inspectResult.status === "rejected"
            ? errorMessage(inspectResult.reason)
            : inspectResult.value === null
              ? "Health inspection is not permitted."
              : errorMessage(inspectResult.value);
      }
      // A valid inspection is retained when only metrics fail.
      if (
        metricsResult.status === "fulfilled" &&
        metricsResult.value !== null &&
        !isErr(metricsResult.value)
      ) {
        healthMetrics = metricsResult.value;
      } else {
        healthMetrics = null;
        metricsError =
          metricsResult.status === "rejected"
            ? errorMessage(metricsResult.reason)
            : metricsResult.value === null
              ? "Health metrics are not permitted."
              : errorMessage(metricsResult.value);
      }
    } finally {
      if (sequence === detailSequence && !disposed) detailLoading = false;
    }
  }

  async function refresh(): Promise<void> {
    await loadParticipants();
    if (disposed) return;
    await loadDetail(selectedParticipant);
  }

  async function selectParticipant(participant: Participant): Promise<void> {
    selectedKey = participantKey(participant);
    error = null;
    try {
      await loadDetail(participant);
    } catch (cause) {
      error = errorMessage(cause);
    }
  }

  function stopWatch() {
    const attempt = watchAttempt;
    watchAttempt = null;
    attempt?.controller.abort();
    attempt?.stream?.close?.();
    void subscription?.dispose();
    subscription = null;
  }

  function startWatch() {
    stopWatch();
    const live = new LiveSubscription({
      subscribe: async () => {
        const attempt = beginOwnedWatch({
          open: (signal) => trellis.healthWatch({}, { signal }).orThrow(),
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
      onStatus: (status, detail) => {
        if (disposed) return;
        watchError =
          status === "reconnecting"
            ? `Health watch disconnected; retry ${detail?.attempt ?? 1} in ${((detail?.retryInMs ?? 1_000) / 1_000).toFixed(0)}s. Manual refresh remains available.`
            : null;
      },
    });
    subscription = live;
    void live.start();
  }

  $effect(() => {
    untrack(() => {
      void refresh()
        .catch((cause) => {
          if (!disposed) error = errorMessage(cause);
        })
        .finally(() => {
          if (!disposed) loading = false;
        });
      startWatch();
    });
    return () => {
      ++loadSequence;
      ++detailSequence;
      stopWatch();
    };
  });

  onDestroy(() => {
    disposed = true;
    ++loadSequence;
    ++detailSequence;
    stopWatch();
    refreshScheduler.dispose();
  });
</script>

<section class="space-y-5">
  <PageToolbar
    title="Health"
    description="Whether your services and devices are running well."
  >
    {#snippet actions()}
      {#if summary}
        <span class="text-xs text-base-content/50"
          >Updated {formatRelativeTime(summary.asOf)}</span
        >
      {/if}
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if watchError}<Notice variant="warning"
      >Live updates paused. {watchError}</Notice
    >{/if}
  {#if summary?.projection.gapDetected}
    <Notice variant="warning"
      >Some health history is missing, so recent changes may not show.</Notice
    >
  {/if}

  {#if loading}
    <LoadingState label="Loading health" />
  {:else}
    <div class="grid grid-cols-2 gap-3 lg:grid-cols-4">
      {#each statusCounts as item (item.status)}
        <div class="rounded-box border border-base-300 bg-base-100 px-4 py-3">
          <div class="text-sm text-base-content/70">
            <HealthStatus status={item.status} />
          </div>
          <div class="mt-1 text-2xl font-semibold tabular-nums">
            {item.count}
          </div>
        </div>
      {/each}
    </div>

    <div class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_28rem]">
      <div class="trellis-section min-w-0">
        <div class="flex flex-wrap items-center justify-between gap-3 p-4">
          <h2 class="text-base font-semibold">Services and devices</h2>
          <div class="trellis-segment" role="group" aria-label="Filter">
            <button
              type="button"
              class={{ active: statusFilter === "all" }}
              aria-pressed={statusFilter === "all"}
              onclick={() => (statusFilter = "all")}>All</button
            >
            <button
              type="button"
              class={{ active: statusFilter === "attention" }}
              aria-pressed={statusFilter === "attention"}
              onclick={() => (statusFilter = "attention")}
              >Needs attention</button
            >
          </div>
        </div>
        {#if visibleParticipants.length === 0}
          <EmptyState
            class="m-4"
            title={participants.length
              ? "Everything is healthy"
              : "Nothing is reporting yet"}
            description={participants.length
              ? "No service or device needs attention."
              : "Services and devices show up after they first report in."}
          />
        {:else}
          <DataTable>
            <thead>
              <tr>
                <th>Name</th>
                <th>Health</th>
                <th>Running</th>
                <th>Version</th>
                <th>Last seen</th>
              </tr>
            </thead>
            <tbody>
              {#each visibleParticipants as participant (participantKey(participant))}
                {@const active =
                  participantKey(participant) ===
                  participantKey(selectedParticipant ?? participant)}
                <tr class={active ? "bg-primary/10" : ""}>
                  <td>
                    <button
                      type="button"
                      class="flex items-center gap-3 text-left"
                      aria-pressed={active}
                      onclick={() => void selectParticipant(participant)}
                    >
                      <span
                        class="grid h-9 w-9 place-items-center rounded-full bg-base-200 text-base-content/70"
                        ><Icon
                          name={participant.participantKind === "device"
                            ? "laptop"
                            : "box"}
                          size={18}
                        /></span
                      >
                      <span>
                        <span class="block font-medium hover:underline"
                          >{humanizeName(participant.participantName)}</span
                        >
                        <span class="block text-xs text-base-content/60"
                          >{formatKind(participant.participantKind)}</span
                        >
                      </span>
                    </button>
                  </td>
                  <td><HealthStatus status={participant.effectiveStatus} /></td>
                  <td class="tabular-nums">{onlineSummary(participant)}</td>
                  <td class="text-base-content/75"
                    >{participant.versions.join(", ") || "—"}</td
                  >
                  <td class="text-base-content/75"
                    >{formatRelativeTime(participant.lastSeenAt)}</td
                  >
                </tr>
              {/each}
            </tbody>
          </DataTable>
          {#if cursorBackStack.length > 0 || nextCursor}
            <Pager
              summary={`Showing ${visibleParticipants.length} on this page`}
              canPrevious={cursorBackStack.length > 0}
              canNext={!!nextCursor}
              onPrevious={goPrevious}
              onNext={goNext}
            />
          {/if}
        {/if}
      </div>

      <div class="trellis-section min-w-0 p-5">
        {#if detailLoading}
          <LoadingState label="Loading details" />
        {:else if inspection && selectedParticipant}
          <div class="flex items-start justify-between gap-3">
            <div class="min-w-0">
              <h2 class="truncate text-lg font-semibold">
                {humanizeName(inspection.participant.participantName)}
              </h2>
              <p class="text-sm text-base-content/60">
                {formatKind(inspection.participant.participantKind)}
              </p>
            </div>
            <HealthStatus status={inspection.participant.effectiveStatus} />
          </div>

          <dl class="mt-4 grid grid-cols-2 gap-3">
            <div class="rounded-field bg-base-200/60 px-3 py-2">
              <dt class="text-xs text-base-content/60">Uptime, last 24h</dt>
              <dd class="text-lg font-semibold">
                {formatAvailability(healthMetrics?.summary.availability)}
              </dd>
            </div>
            <div class="rounded-field bg-base-200/60 px-3 py-2">
              <dt class="text-xs text-base-content/60">Running</dt>
              <dd class="text-lg font-semibold">
                {onlineSummary(inspection.participant)}
              </dd>
            </div>
          </dl>
          {#if metricsError}<p class="mt-2 text-xs text-base-content/60">
              Uptime isn't available: {metricsError}
            </p>{/if}

          {#if selectedInstance}
            <h3 class="mt-5 text-sm font-semibold">Checks</h3>
            {#if selectedInstance.latestSample.checks.length === 0}
              <p class="mt-1 text-sm text-base-content/60">
                This {formatKind(
                  inspection.participant.participantKind,
                ).toLowerCase()} doesn't report any checks.
              </p>
            {:else}
              <ul
                class="mt-2 divide-y divide-base-300 rounded-field border border-base-300"
              >
                {#each selectedInstance.latestSample.checks as check (check.name)}
                  <li
                    class="flex items-center justify-between gap-3 px-3 py-2 text-sm"
                  >
                    <span class="min-w-0">
                      <span class="block font-medium"
                        >{humanizeName(check.name)}</span
                      >
                      {#if check.summary}<span
                          class="block truncate text-xs text-base-content/60"
                          >{check.summary}</span
                        >{/if}
                      {#if check.error}<span class="block text-xs text-error"
                          >{check.error}</span
                        >{/if}
                      {#each infoEntries(check.info) as [label, value] (label)}<span
                          class="block text-xs text-base-content/60"
                          >{label}: {value}</span
                        >{/each}
                    </span>
                    <span class="flex items-center gap-3">
                      <span class="text-xs text-base-content/60 tabular-nums"
                        >{check.latencyMs.toFixed(0)} ms</span
                      >
                      <StatusBadge
                        label={check.status === "ok" ? "OK" : check.status}
                        status={check.status === "ok" ? "healthy" : "unhealthy"}
                      />
                    </span>
                  </li>
                {/each}
              </ul>
            {/if}
          {/if}

          <h3 class="mt-5 text-sm font-semibold">Recent changes</h3>
          <ol class="mt-2 space-y-2">
            {#each inspection.history.slice(0, 8) as interval (interval.intervalId)}
              <li class="flex items-start gap-3 text-sm">
                <span class="w-36 shrink-0"
                  ><HealthStatus status={interval.effectiveStatus} /></span
                >
                <span class="min-w-0 text-base-content/70">
                  {formatRelativeTime(interval.startedAt)}{interval.endedAt
                    ? ""
                    : " · now"}
                  {#if interval.reason}<span class="block truncate text-xs"
                      >{changeReason(interval.reason)}</span
                    >{/if}
                </span>
              </li>
            {/each}
          </ol>

          {#if selectedInstance}
            {@const reported = selectedInstance.latestSample.participant}
            <h3 class="mt-5 text-sm font-semibold">About this instance</h3>
            <dl
              class="mt-2 grid grid-cols-[7rem_minmax(0,1fr)] gap-x-3 gap-y-1.5 text-sm"
            >
              {#if selectedInstance.latestSample.summary}
                <dt class="text-base-content/60">Status</dt>
                <dd>{selectedInstance.latestSample.summary}</dd>
              {/if}
              {#if reported.version}
                <dt class="text-base-content/60">Version</dt>
                <dd>{reported.version}</dd>
              {/if}
              <dt class="text-base-content/60">Runs on</dt>
              <dd>
                {humanizeName(reported.runtime)}{reported.runtimeVersion
                  ? ` ${reported.runtimeVersion}`
                  : ""}
              </dd>
              <dt class="text-base-content/60">Started</dt>
              <dd>{formatRelativeTime(reported.startedAt)}</dd>
              <dt class="text-base-content/60">Last report</dt>
              <dd>{formatRelativeTime(selectedInstance.observedAt)}</dd>
              <dt class="text-base-content/60">Next report by</dt>
              <dd>{formatDate(selectedInstance.heartbeatDeadline)}</dd>
              {#each infoEntries(reported.info) as [label, value] (label)}
                <dt class="truncate text-base-content/60">{label}</dt>
                <dd class="break-words">{value}</dd>
              {/each}
            </dl>
          {/if}
        {:else}
          <p class="py-10 text-center text-sm text-base-content/60">
            Choose a service or device to see its details.
          </p>
        {/if}
      </div>
    </div>
  {/if}
</section>
