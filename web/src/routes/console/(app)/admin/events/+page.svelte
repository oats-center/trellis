<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import MetricsLedger from "$lib/components/MetricsLedger.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { boundedNumber, compactDuration, errorMessage, formatDate, jsonBlock } from "$lib/format";
  import { getConnection, getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import {
    consumerSeverityOf,
    consumerStatus,
    type ConsumerStatus,
    DetailOwnership,
    type OptionalReadState,
    projectEventsHealthLedger,
    runDeadLetterMutation,
  } from "$lib/console/events_detail.ts";
  import {
    beginOwnedWatch,
    LiveSubscription,
    RefreshScheduler,
    type WatchAttempt,
  } from "$lib/console/live_refresh.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { type apis } from "trellis-web-generated";
  import { subjectMatches } from "./subject";

  type WindowValue = "15m" | "1h" | "6h" | "24h" | "7d";
  type EventResolution = "resolved" | "unresolved" | "malformed";
  type EventVerificationStatus =
    | "verified"
    | "missing-proof"
    | "missing-key"
    | "untrusted-key"
    | "invalid-signature"
    | "missing-session"
    | "subject-denied"
    | "outside-session-window"
    | "auth-unavailable";
  type Focus = "exceptions" | "all" | "unresolved" | "malformed" | "largest" | EventVerificationStatus;
  type EventTypeRef = { ownerContractId: string; ownerEventName: string };
  type ConsumerManagedBy = "authority" | "platform" | "external";

  type EventRow = apis.events.QueryOutput["items"][number];

  type EventInspect = {
    event: EventRow;
    headers: Record<string, string>;
    payload?: Uint8Array;
    payloadText?: string;
    decodeError?: string;
    related: Array<{ eventId: string; eventTime: string; subject: string; matchedBy: string }>;
  };

  type ConsumerRow = {
    resourceId: string;
    deploymentId?: string;
    contractId?: string;
    group?: string;
    stream: string;
    consumerName: string;
    filterSubjects: string[];
    managedBy: ConsumerManagedBy;
    status: ConsumerStatus;
    pending: number;
    ackPending: number;
    waitingPulls: number;
    redelivered?: number;
    concurrency?: number;
    ackWaitMs?: number;
    maxDeliver?: number;
    oldestPendingAt?: string;
    oldestPendingEventId?: string;
  };

  const trellis = getTrellis();
  const connection = getConnection();
  const authority = getConsoleAuthority();
  const rpcTimeout = 10_000;
  const pageLimit = 40;
  const windows: Array<{ value: WindowValue; label: string; minutes: number }> = [
    { value: "15m", label: "15m", minutes: 15 },
    { value: "1h", label: "1h", minutes: 60 },
    { value: "6h", label: "6h", minutes: 360 },
    { value: "24h", label: "24h", minutes: 1_440 },
    { value: "7d", label: "7d", minutes: 10_080 },
  ];
  const verificationIssues: EventVerificationStatus[] = [
    "missing-proof",
    "missing-key",
    "untrusted-key",
    "invalid-signature",
    "missing-session",
    "subject-denied",
    "outside-session-window",
    "auth-unavailable",
  ];

  let loading = $state(true);
  let refreshing = $state(false);
  let error = $state<string | null>(null);
  let unavailableMessage = $state<string | null>(null);
  let feedOnline = $state(false);
  let feedMessage = $state<string | null>(null);
  let rows = $state.raw<EventRow[]>([]);
  let consumers = $state.raw<ConsumerRow[]>([]);
  let metrics = $state.raw<apis.events.MetricsOutput | null>(null);
  let diagnostics = $state.raw<apis.events.DiagnosticsOutput | null>(null);
  let deadLetters = $state.raw<apis.events.DeadLettersQueryOutput["items"]>([]);
  let selectedDeadLetter = $state.raw<apis.events.DeadLettersInspectOutput["deadLetter"] | null>(null);
  let deadLetterError = $state<Record<string, string>>({});
  let deadLetterBusy = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  let selectedEvent = $state.raw<EventInspect | null>(null);
  let selectedConsumer = $state.raw<{ row: ConsumerRow; detail: Record<string, unknown> | null } | null>(null);
  let detailLoading = $state(false);
  let detailError = $state<string | null>(null);
  let focus = $state<Focus>(asEventFocus(page.url.searchParams.get("focus")) ?? "exceptions");
  let handledFocusParam = page.url.searchParams.get("focus");
  /**
   * Completion status of each optional read, separate from its value.
   *
   * A successful empty result is ready/empty; a failed, denied, or
   * not-yet-completed read is not, and must never be presented as a numeric
   * zero or a healthy-empty state for the same data.
   */
  let metricsState = $state<OptionalReadState>("pending");
  let consumersState = $state<OptionalReadState>("pending");
  /** The window the retained metrics belong to, so a window change cannot show them. */
  let metricsScope = $state<WindowValue | null>(null);

  afterNavigate(() => {
    const value = page.url.searchParams.get("focus");
    if (value === handledFocusParam) return;
    handledFocusParam = value;
    const next = asEventFocus(value);
    if (next && next !== focus) {
      focus = next;
      cursor = undefined;
      cursorBackStack = [];
      invalidateDetails();
      void requestSnapshot();
    }
  });
  let selectedEventType = $state.raw<EventTypeRef | null>(null);
  let attentionConsumersOnly = $state(false);
  let searchText = $state("");
  let ownerContractId = $state("");
  let publisherDeploymentId = $state("");
  let windowValue = $state<WindowValue>("1h");
  let cursor = $state<string | undefined>();
  let cursorBackStack = $state<string[]>([]);
  let nextCursor = $state<string | undefined>();
  let lastUpdated = $state<Date | null>(null);

  let loadSequence = 0;
  let deadLetterSequence = 0;
  let deadLetterBusyGeneration = 0;
  let disposed = false;
  let queryEpoch = 0;
  let selectionEpoch = 0;
  /**
   * Semantic identity of the committed list query. Changing focus, filters,
   * window, or page invalidates every detail read that belonged to the old
   * query, so a late response cannot repopulate the detail area.
   */
  let listKey = $state("");
  type DesiredQuery = {
    key: string;
    eventQuery: apis.events.QueryInput;
    metricsInput: apis.events.MetricsInput;
    window: WindowValue;
  };
  let desiredQuery: DesiredQuery = {
    key: "",
    eventQuery: { page: { limit: pageLimit }, window: "1h" },
    metricsInput: { window: "1h" },
    window: "1h",
  };
  const detailOwnership = new DetailOwnership();
  let watchAttempt: WatchAttempt<unknown> | null = null;
  let subscription: LiveSubscription | null = null;
  let consumerError = $state<string | null>(null);
  let metricsError = $state<string | null>(null);
  let diagnosticsError = $state<string | null>(null);
  let deadLetterPanelError = $state<string | null>(null);
  let snapshotShowsLoading = true;
  const refreshScheduler = new RefreshScheduler({
    canRefresh: () =>
      !disposed &&
      document.visibilityState === "visible" &&
      connection.status.phase === "connected",
    onRefresh: () => {
      const showLoading = snapshotShowsLoading;
      snapshotShowsLoading = false;
      return readSnapshot(showLoading);
    },
    onRefreshError: (cause) => { error = errorMessage(cause); },
  });

  const windowOption = $derived(windows.find((option) => option.value === windowValue) ?? windows[1]);
  const visibleMetricsState = $derived<OptionalReadState>(
    metricsScope === windowValue ? metricsState : "pending",
  );
  const metricsReady = $derived(visibleMetricsState === "ready" && metrics !== null);
  const eventRate = $derived(
    metricsReady && metrics
      ? boundedNumber(metrics.summary.total) / windowOption.minutes
      : null,
  );
  const averagePayload = $derived(
    metricsReady && metrics && metrics.summary.total
      ? boundedNumber(metrics.summary.payloadSizeBytes) / boundedNumber(metrics.summary.total)
      : null,
  );
  const eventTypesByCount = $derived.by(() => [...(metrics?.summary.eventTypes ?? [])].sort((a, b) => boundedNumber(b.count - a.count)));
  const matchingConsumers = $derived.by(() => {
    const subject = selectedEvent?.event.subject;
    return subject
      ? consumers.filter((consumer) => consumer.filterSubjects.some((filter) => subjectMatches(filter, subject)))
      : [];
  });
  const sortedConsumers = $derived.by(() =>
    [...consumers].sort((a, b) =>
      consumerSeverityOf(a.status) - consumerSeverityOf(b.status)
    )
  );
  const attentionConsumers = $derived(sortedConsumers.filter(isAttentionConsumer));
  const displayedConsumers = $derived(attentionConsumersOnly ? attentionConsumers : sortedConsumers);
  const oldestLagConsumer = $derived.by(() =>
    consumers
      .filter((consumer) => consumer.oldestPendingAt)
      .sort((a, b) => new Date(a.oldestPendingAt ?? 0).getTime() - new Date(b.oldestPendingAt ?? 0).getTime())[0],
  );
  const eventPoints = $derived(
    metricsReady && metrics
      ? chartPoints(metrics.buckets.map((bucket) => boundedNumber(bucket.total)))
      : "",
  );
  const exceptionPoints = $derived(
    metricsReady && metrics
      ? chartPoints(metrics.buckets.map((bucket) => boundedNumber(bucket.integrityExceptions)))
      : "",
  );
  const focusTitle = $derived(`${selectedEventType ? `${selectedEventType.ownerEventName} · ` : ""}${focusLabel(focus)}`);
  const focusDescription = $derived(focusDetail(focus));

  function objectRecord(value: unknown): Record<string, unknown> {
    return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
  }

  function stringValue(value: unknown): string | undefined {
    return typeof value === "string" && value.length > 0 ? value : undefined;
  }

  function numberValue(value: unknown): number | undefined {
    if (typeof value === "bigint") return boundedNumber(value);
    return typeof value === "number" && Number.isFinite(value) ? value : undefined;
  }

  function stringArray(value: unknown): string[] {
    return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];
  }

  function isResolution(value: unknown): value is EventResolution {
    return value === "resolved" || value === "unresolved" || value === "malformed";
  }

  function isVerificationStatus(value: unknown): value is EventVerificationStatus {
    return value === "verified" || verificationIssues.includes(value as EventVerificationStatus);
  }


  function isConsumerManagedBy(value: unknown): value is ConsumerManagedBy {
    return value === "authority" || value === "platform" || value === "external";
  }

  function toConsumerRow(value: unknown): ConsumerRow | null {
    const row = objectRecord(value);
    const stream = stringValue(row.stream);
    const consumerName = stringValue(row.consumerName);
    if (!stream || !consumerName) return null;
    const managedBy = isConsumerManagedBy(row.managedBy) ? row.managedBy : stringValue(row.deploymentId) ? "authority" : "external";
    return {
      resourceId: stringValue(row.resourceId) ?? "",
      deploymentId: stringValue(row.deploymentId),
      contractId: stringValue(row.contractId),
      group: stringValue(row.group),
      stream,
      consumerName,
      filterSubjects: stringArray(row.filterSubjects),
      managedBy,
      status: managedBy === "external" ? "unmanaged" : consumerStatus(row.status),
      pending: numberValue(row.pending) ?? 0,
      ackPending: numberValue(row.ackPending) ?? 0,
      waitingPulls: numberValue(row.waitingPulls) ?? 0,
      redelivered: numberValue(row.redelivered),
      concurrency: numberValue(row.concurrency),
      ackWaitMs: numberValue(row.ackWaitMs),
      maxDeliver: numberValue(row.maxDeliver),
      oldestPendingAt: stringValue(row.oldestPendingAt),
      oldestPendingEventId: stringValue(row.oldestPendingEventId),
    };
  }

  function toInspect(value: apis.events.InspectOutput): EventInspect {
    const payload = value.event.payload;
    try {
      return { event: value.event.row, headers: value.event.headers, payload, payloadText: new TextDecoder("utf-8", { fatal: true }).decode(payload), related: [] };
    } catch {
      return { event: value.event.row, headers: value.event.headers, payload, payloadText: Array.from(payload, (byte) => byte.toString(16).padStart(2, "0")).join(" "), decodeError: "Payload is not valid UTF-8; showing hexadecimal bytes.", related: [] };
    }
  }

  function isAttentionConsumer(consumer: ConsumerRow): boolean {
    return consumer.status !== "current" && consumer.status !== "processing" &&
      consumer.status !== "unmanaged";
  }

  function statusBadgeClass(status: string): string {
    if (status === "verified" || status === "current") return "badge-success";
    if (status === "processing" || status === "behind" || status === "saturated" || status === "orphaned" || status === "unresolved") return "badge-warning";
    if (status === "missing-proof" || status === "auth-unavailable" || status === "inactive" || status === "unmanaged" || status.startsWith("unknown:")) return "badge-neutral";
    return "badge-error";
  }

  function verificationCount(status: EventVerificationStatus): bigint {
    const counts = metrics?.summary.byVerificationStatus;
    if (!counts) return 0n;
    if (status === "missing-proof") return counts.missingProof ?? 0n;
    if (status === "invalid-signature") return counts.invalidSignature ?? 0n;
    if (status === "missing-session") return counts.missingSession ?? 0n;
    if (status === "subject-denied") return counts.subjectDenied ?? 0n;
    if (status === "outside-session-window") return counts.outsideSessionWindow ?? 0n;
    if (status === "auth-unavailable") return counts.authUnavailable ?? 0n;
    if (status === "verified") return counts.verified ?? 0n;
    return 0n;
  }

  function consumerVariant(consumer: ConsumerRow): "healthy" | "degraded" | "unhealthy" | "offline" {
    if (consumer.status === "current" || consumer.status === "processing") return "healthy";
    if (consumer.status === "behind" || consumer.status === "saturated") return "degraded";
    if (consumer.status === "unmanaged" || consumer.status.startsWith("unknown:")) return "offline";
    return "unhealthy";
  }

  const ledgerItems = $derived(projectEventsHealthLedger({
    metricsState: visibleMetricsState,
    consumersState,
    total: metricsReady && metrics ? metrics.summary.total : undefined,
    integrityExceptions: metricsReady && metrics ? metrics.summary.integrityExceptions : undefined,
    unresolved: metricsReady && metrics ? metrics.summary.byResolution.unresolved : undefined,
    payloadTotalLabel: formatBytes(
      metricsReady && metrics ? boundedNumber(metrics.summary.payloadSizeBytes) : 0,
    ),
    payloadAverageLabel: formatBytes(averagePayload ?? 0),
    windowMinutes: windowOption.minutes,
    attentionConsumers: attentionConsumers.length,
    shownConsumers: consumers.length,
    oldestLagValue: ageLabel(oldestLagConsumer?.oldestPendingAt),
    oldestLagDetail: oldestLagConsumer?.consumerName ?? "no pending events",
    oldestLagDisabled: !oldestLagConsumer,
    focus,
    attentionConsumersOnly,
    oldestLagActive: selectedConsumer?.row.consumerName === oldestLagConsumer?.consumerName,
  }));

  function handleLedgerSelect(id: string) {
    if (id === "consumers") showAttentionConsumers();
    else if (id === "oldest-lag") selectOldestLag();
    else if (id === "all" || id === "exceptions" || id === "unresolved" || id === "largest") selectFocus(id);
  }

  function formatBytes(bytes: number): string {
    if (bytes < 1024) return `${Math.round(bytes)} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
  }

  function ageLabel(value: string | undefined): string {
    if (!value) return "-";
    const time = new Date(value).getTime();
    return Number.isNaN(time) ? value : compactDuration(Date.now() - time);
  }

  function subjectOwner(row: EventRow): string {
    if (row.ownerContractId && row.ownerEventName) return `${row.ownerContractId} / ${row.ownerEventName}`;
    return row.ownerContractId ?? row.ownerEventName ?? row.resolution;
  }

  function publisherLabel(row: EventRow): string {
    return row.publisherDeploymentId ?? row.publisherParticipantId ?? row.publisherKind ?? "unverified";
  }

  function focusLabel(value: Focus): string {
    if (value === "exceptions") return "Recent integrity exceptions";
    if (value === "all") return "Recent event flow";
    if (value === "unresolved") return "Unresolved events";
    if (value === "malformed") return "Malformed events";
    if (value === "largest") return "Largest event payloads";
    return `${value.replaceAll("-", " ")} events`;
  }

  function focusDetail(value: Focus): string {
    if (value === "exceptions") return "Events that could not be fully attributed or verified.";
    if (value === "all") return "Newest projected events in the selected window.";
    if (value === "unresolved") return "Subjects that could not be associated with a catalog event.";
    if (value === "malformed") return "Event envelopes the projector could not interpret.";
    if (value === "largest") return "Highest payload sizes in the selected window.";
    return `Events with verification status ${value}.`;
  }

  function chartPoints(values: number[]): string {
    if (values.length === 0) return "";
    const maximum = Math.max(...values, 1);
    return values.map((value, index) => `${values.length === 1 ? 130 : index * 260 / (values.length - 1)},${52 - value / maximum * 46}`).join(" ");
  }

  function buildEventQuery(): apis.events.QueryInput {
    const input: apis.events.QueryInput = {
      includeEventTypes: selectedEventType ? [selectedEventType] : undefined,
      page: { cursor, limit: pageLimit },
      ownerContractId: ownerContractId.trim() || undefined,
      publisherDeploymentId: publisherDeploymentId.trim() || undefined,
      search: searchText.trim() || undefined,
      sort: { field: focus === "largest" ? "payloadSize" : "eventTime", direction: "desc" },
      window: windowValue,
    };
    if (focus === "exceptions") return { ...input, integrityExceptionOnly: true };
    if (focus === "unresolved" || focus === "malformed") return { ...input, resolution: [focus] };
    if (isVerificationStatus(focus)) return { ...input, verificationStatus: [focus] };
    return input;
  }

  function buildConsumerQuery(): apis.events.ConsumersQueryInput {
    return { page: { limit: 500 } };
  }

  async function loadConsumers(): Promise<apis.events.ConsumersQueryOutput["items"]> {
    const items: apis.events.ConsumersQueryOutput["items"] = [];
    for await (const item of trellis.consumersQuery.items(buildConsumerQuery(), { timeout: rpcTimeout })) {
      items.push(item.orThrow());
    }
    return items;
  }

  async function loadDeadLetters(resourceId: string): Promise<apis.events.DeadLettersQueryOutput["items"]> {
    const items: apis.events.DeadLettersQueryOutput["items"] = [];
    for await (const item of trellis.deadLettersQuery.items({ resourceId, state: ["dead", "replayPending", "replaying"], page: { limit: 500 } }, { timeout: rpcTimeout })) {
      items.push(item.orThrow());
    }
    return items;
  }

  function unavailableText(loadError: unknown): string | null {
    const message = errorMessage(loadError);
    const normalized = message.toLowerCase();
    if (normalized.includes("no responders") || normalized.includes("not currently reachable") || normalized.includes("inactive contract")) {
      return "Events management is unavailable; event history, managed Consumer DLQ, and replay cannot be inspected.";
    }
    if (normalized.includes("permissions violation")) {
      return "Your current session is not approved for Events access. Sign out and sign back in to refresh permissions.";
    }
    return null;
  }

  function commitQueryChange(): void {
    const key = currentListKey();
    if (key === desiredQuery.key && key === listKey) return;
    queryEpoch += 1;
    listKey = key;
    desiredQuery = {
      key,
      eventQuery: buildEventQuery(),
      metricsInput: { window: windowValue },
      window: windowValue,
    };
    detailOwnership.setListKey(key);
  }

  function requestSnapshot(showLoading = true) {
    commitQueryChange();
    if (showLoading) snapshotShowsLoading = true;
    return refreshScheduler.refreshNow();
  }

  async function readSnapshot(showLoading = true) {
    if (disposed) return;
    const sequence = ++loadSequence;
    const capturedQuery = desiredQuery;
    const capturedEpoch = queryEpoch;
    const capturedSelectionEpoch = selectionEpoch;
    const requestedWindow = capturedQuery.window;
    if (showLoading) loading = true;
    else refreshing = true;
    const selectedResourceId = selectedConsumer?.row.resourceId;
    // The event query is primary. Consumer query, metrics, diagnostics, and the
    // dead-letter query are independent: a denied or failed optional read must
    // not blank permitted event rows.
    const [eventResult, consumerResult, metricsResult, diagnosticsResult, deadLetterResult] =
      await Promise.allSettled([
        trellis.eventsQuery(capturedQuery.eventQuery, { timeout: rpcTimeout }).orThrow(),
        loadConsumers(),
        trellis.eventsMetrics(capturedQuery.metricsInput, { timeout: rpcTimeout }).orThrow(),
        trellis.diagnostics({}, { timeout: rpcTimeout }).orThrow(),
        selectedResourceId ? loadDeadLetters(selectedResourceId) : Promise.resolve(null),
      ]);
    if (
      disposed ||
      sequence !== loadSequence ||
      capturedEpoch !== queryEpoch ||
      capturedQuery.key !== desiredQuery.key
    ) return;
    try {
      if (eventResult.status === "fulfilled") {
        error = null;
        unavailableMessage = null;
        rows = eventResult.value.items;
        nextCursor = eventResult.value.page.nextCursor;
        lastUpdated = new Date();
      } else {
        const unavailable = unavailableText(eventResult.reason);
        if (unavailable) {
          unavailableMessage = unavailable;
          rows = [];
          nextCursor = undefined;
        } else {
          error = errorMessage(eventResult.reason);
          rows = [];
          nextCursor = undefined;
        }
      }
      if (consumerResult.status === "fulfilled" && consumerResult.value !== null) {
        consumers = consumerResult.value
          .map(toConsumerRow)
          .filter((row): row is ConsumerRow => row !== null);
        consumersState = "ready";
        consumerError = null;
      } else {
        consumers = [];
        consumersState = "unavailable";
        consumerError = consumerResult.status === "rejected" ? errorMessage(consumerResult.reason) : "Consumer query is not permitted.";
      }
      if (metricsResult.status === "fulfilled" && metricsResult.value !== null) {
        metrics = metricsResult.value;
        metricsState = "ready";
        metricsScope = requestedWindow;
        metricsError = null;
      } else {
        metrics = null;
        metricsState = "unavailable";
        metricsScope = requestedWindow;
        metricsError = metricsResult.status === "rejected" ? errorMessage(metricsResult.reason) : "Event metrics are not permitted.";
      }
      if (diagnosticsResult.status === "fulfilled" && diagnosticsResult.value !== null) {
        diagnostics = diagnosticsResult.value;
        diagnosticsError = null;
      } else {
        diagnostics = null;
        diagnosticsError = diagnosticsResult.status === "rejected" ? errorMessage(diagnosticsResult.reason) : "Event diagnostics are not permitted.";
      }
      if (
        capturedSelectionEpoch === selectionEpoch &&
        deadLetterResult.status === "fulfilled" &&
        deadLetterResult.value !== null &&
        selectedResourceId === selectedConsumer?.row.resourceId
      ) {
        deadLetters = deadLetterResult.value;
        deadLetterPanelError = null;
      } else if (
        capturedSelectionEpoch === selectionEpoch &&
        selectedResourceId === selectedConsumer?.row.resourceId
      ) {
        deadLetters = [];
        if (selectedResourceId) deadLetterPanelError = deadLetterResult.status === "rejected" ? errorMessage(deadLetterResult.reason) : "Dead-letter query is not permitted.";
      }
    } finally {
      if (
        sequence === loadSequence &&
        !disposed &&
        capturedEpoch === queryEpoch &&
        capturedQuery.key === desiredQuery.key
      ) {
        loading = false;
        refreshing = false;
      }
    }
  }

  function currentListKey(): string {
    return JSON.stringify([
      focus,
      selectedEventType?.ownerContractId ?? null,
      selectedEventType?.ownerEventName ?? null,
      ownerContractId,
      publisherDeploymentId,
      searchText.trim(),
      windowValue,
      cursor ?? null,
    ]);
  }

  /**
   * Ends every detail read that belonged to the previous list query. Call this
   * before starting the next list read whenever the semantic query changes.
   */
  function invalidateDetails(): void {
    detailOwnership.invalidate();
    selectionEpoch += 1;
    ++deadLetterSequence;
    selectedEvent = null;
    selectedConsumer = null;
    selectedDeadLetter = null;
    detailError = null;
    detailLoading = false;
    deadLetters = [];
    deadLetterError = {};
    deadLetterPanelError = null;
  }

  function resetAndLoad() {
    cursor = undefined;
    cursorBackStack = [];
    invalidateDetails();
    void requestSnapshot();
  }

  function selectFocus(value: Focus) {
    focus = value;
    resetAndLoad();
  }

  function asEventFocus(value: string | null): Focus | null {
    if (value === "all" || value === "exceptions" || value === "unresolved" || value === "malformed" || value === "largest") return value;
    if (isVerificationStatus(value)) return value;
    return null;
  }

  function selectEventType(eventType: EventTypeRef) {
    selectedEventType = selectedEventType?.ownerContractId === eventType.ownerContractId && selectedEventType.ownerEventName === eventType.ownerEventName ? null : eventType;
    resetAndLoad();
  }

  function clearEventFilters() {
    searchText = "";
    ownerContractId = "";
    publisherDeploymentId = "";
    selectedEventType = null;
    focus = "exceptions";
    resetAndLoad();
  }

  function showAttentionConsumers() {
    attentionConsumersOnly = !attentionConsumersOnly;
    document.querySelector(".consumer-health")?.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  function selectOwner(owner: string | undefined) {
    if (!owner) return;
    ownerContractId = owner;
    resetAndLoad();
  }

  function selectPublisher(deployment: string | undefined) {
    if (!deployment) return;
    publisherDeploymentId = deployment;
    resetAndLoad();
  }

  async function inspectEvent(row: EventRow) {
    const token = detailOwnership.begin("event", row.eventId ?? String(row.streamSequence));
    ++deadLetterSequence;
    // Selecting B clears A's visible detail synchronously.
    detailLoading = true;
    detailError = null;
    selectedConsumer = null;
    selectedDeadLetter = null;
    deadLetters = [];
    selectedEvent = null;
    try {
      const input: apis.events.InspectInput = row.eventId ? { eventId: row.eventId } : { streamSequence: row.streamSequence };
      const detail = toInspect(await trellis.eventsInspect(input, { timeout: rpcTimeout }).orThrow());
      if (!detailOwnership.owns(token) || disposed) return;
      selectedEvent = detail;
    } catch (inspectError) {
      if (!detailOwnership.owns(token) || disposed) return;
      detailError = errorMessage(inspectError);
      selectedEvent = { event: row, headers: {}, related: [] };
    } finally {
      if (detailOwnership.owns(token) && !disposed) detailLoading = false;
    }
  }

  async function inspectConsumer(row: ConsumerRow) {
    selectionEpoch += 1;
    const token = detailOwnership.begin("consumer", row.consumerName);
    ++deadLetterSequence;
    detailLoading = true;
    detailError = null;
    selectedEvent = null;
    selectedDeadLetter = null;
    deadLetters = [];
    try {
      const [detail, consumerDeadLetters] = await Promise.allSettled([
        trellis.consumersInspect({ resourceId: row.resourceId }, { timeout: rpcTimeout }).orThrow(),
        loadDeadLetters(row.resourceId),
      ]);
      // The selection may have changed while the read was in flight; only the
      // consumer the page still has selected may publish a detail or its DLQ.
      if (!detailOwnership.owns(token) || disposed) return;
      selectedConsumer = { row, detail: detail.status === "fulfilled" && detail.value !== null ? objectRecord(detail.value) : null };
      detailError = detail.status === "rejected" ? errorMessage(detail.reason) : detail.value === null ? "Consumer inspection is not permitted." : null;
      deadLetters = consumerDeadLetters.status === "fulfilled" ? consumerDeadLetters.value ?? [] : [];
      deadLetterPanelError = consumerDeadLetters.status === "rejected" ? errorMessage(consumerDeadLetters.reason) : consumerDeadLetters.value === null ? "Dead-letter query is not permitted." : null;
    } catch (inspectError) {
      if (!detailOwnership.owns(token) || disposed) return;
      detailError = errorMessage(inspectError);
      selectedConsumer = { row, detail: null };
    } finally {
      if (detailOwnership.owns(token) && !disposed) detailLoading = false;
    }
  }

  async function inspectDeadLetter(deadLetter: apis.events.DeadLettersQueryOutput["items"][number]) {
    const sequence = ++deadLetterSequence;
    deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: "" };
    try {
      const result = await trellis.deadLettersInspect({ resourceId: deadLetter.resourceId, deadLetterId: deadLetter.deadLetterId }, { timeout: rpcTimeout }).orThrow();
      if (sequence === deadLetterSequence && !disposed && selectedConsumer?.row.resourceId === deadLetter.resourceId) selectedDeadLetter = result.deadLetter;
    } catch (cause) {
      if (sequence === deadLetterSequence && !disposed) deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: errorMessage(cause) };
    }
  }

  async function changeDeadLetter(deadLetter: apis.events.DeadLettersQueryOutput["items"][number], action: "replay" | "dismiss") {
    const resourceId = selectedConsumer?.row.resourceId;
    if (resourceId !== deadLetter.resourceId) return;
    const capturedQueryEpoch = queryEpoch;
    const capturedKey = desiredQuery.key;
    const capturedSelectionEpoch = selectionEpoch;
    const mutationToken = ++deadLetterBusyGeneration;
    const owned = () =>
      !disposed &&
      capturedQueryEpoch === queryEpoch &&
      capturedKey === desiredQuery.key &&
      capturedSelectionEpoch === selectionEpoch;
    const input = { resourceId: deadLetter.resourceId, deadLetterId: deadLetter.deadLetterId, expectedRevision: deadLetter.revision, requestId: crypto.randomUUID() };
    const confirmed = await confirmationModal?.confirm({
      title: `${action === "replay" ? "Replay" : "Dismiss"} dead letter?`,
      message: action === "replay" ? "The original event will be queued for another delivery attempt." : "The dead letter will be marked dismissed without redelivery.",
      confirmLabel: action === "replay" ? "Replay" : "Dismiss",
      targetLabel: "Dead letter",
      targetName: deadLetter.deadLetterId,
    });
    if (!confirmed || !owned() || selectedConsumer?.row.resourceId !== resourceId ||
      !deadLetters.some((item) => item.deadLetterId === input.deadLetterId && item.revision === input.expectedRevision)) return;
    deadLetterBusy = deadLetter.deadLetterId;
    deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: "" };
    try {
      await runDeadLetterMutation({
        mounted: owned,
        mutate: async () => {
          if (action === "replay") await trellis.deadLettersReplay(input, { timeout: rpcTimeout }).orThrow();
          else await trellis.deadLettersDismiss(input, { timeout: rpcTimeout }).orThrow();
        },
        followUp: () => {
          if (!owned()) return;
          return requestSnapshot(false);
        },
        onError: (cause) => {
          if (!owned() || selectedConsumer?.row.resourceId !== resourceId) return;
          deadLetterError = {
            ...deadLetterError,
            [deadLetter.deadLetterId]: classifyMutationError(cause).kind === "unknown"
              ? "Outcome unknown. Inspect this dead letter before attempting another action."
              : errorMessage(cause),
          };
        },
      });
    } finally {
      if (owned() && deadLetterBusy === mutationToken) deadLetterBusy = null;
    }
  }

  function selectOldestLag() {
    if (oldestLagConsumer) void inspectConsumer(oldestLagConsumer);
  }

  function goPrevious() {
    cursor = cursorBackStack.pop() || undefined;
    invalidateDetails();
    void requestSnapshot();
  }

  function goNext() {
    if (!nextCursor) return;
    cursorBackStack.push(cursor ?? "");
    cursor = nextCursor;
    invalidateDetails();
    void requestSnapshot();
  }

  function startWatch() {
    stopWatch();
    const live = new LiveSubscription({
      subscribe: async () => {
        const attempt = beginOwnedWatch({
          open: (signal) => trellis.eventsWatch({}, { signal }).orThrow(),
          onFrame: (frame) => {
            if (objectRecord(frame).kind !== "ready") refreshScheduler.notify();
          },
          stillOwned: () => !disposed && watchAttempt === attempt,
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
        feedOnline = status === "live";
        feedMessage = status === "reconnecting"
          ? `Live feed disconnected; retry ${detail?.attempt ?? 1} in ${((detail?.retryInMs ?? 1_000) / 1_000).toFixed(0)}s. Manual refresh remains available.`
          : null;
      },
    });
    subscription = live;
    void live.start();
  }

  function stopWatch() {
    const attempt = watchAttempt;
    watchAttempt = null;
    attempt?.controller.abort();
    attempt?.stream?.close?.();
    void subscription?.dispose();
    subscription = null;
    feedOnline = false;
  }

  /** A hidden page or a recovered connection resumes one retained refresh. */
  function handleVisibilityChange(): void {
    if (document.visibilityState === "visible") refreshScheduler.resume();
  }

  $effect(() => {
    untrack(() => {
      void requestSnapshot();
      startWatch();
    });
    return () => {
      ++loadSequence;
      detailOwnership.invalidate();
      ++deadLetterSequence;
      stopWatch();
    };
  });

  $effect(() => {
    if (connection.status.phase === "connected") {
      untrack(() => refreshScheduler.resume());
    }
  });

  onDestroy(() => {
    disposed = true;
    ++loadSequence;
    detailOwnership.invalidate();
    ++deadLetterSequence;
    stopWatch();
    refreshScheduler.dispose();
  });
</script>

<svelte:document onvisibilitychange={handleVisibilityChange} />

<section class="events-page">
  <PageToolbar title="Events" description="Delivery health, event integrity, and recent exceptions.">
    {#snippet meta()}
      <span class={['badge badge-sm', feedOnline ? 'badge-success' : 'badge-neutral']}>{feedOnline ? "Live" : "Historical"}</span>
      {#if lastUpdated}<span class="text-xs text-base-content/50">Updated {lastUpdated.toLocaleTimeString()}</span>{/if}
    {/snippet}
    {#snippet actions()}
      <div class="trellis-segment" role="group" aria-label="Metrics window">
        {#each windows as option (option.value)}
          <button type="button" class:active={windowValue === option.value} aria-pressed={windowValue === option.value} onclick={() => { windowValue = option.value; resetAndLoad(); }}>{option.label}</button>
        {/each}
      </div>
      <button class="btn btn-ghost btn-sm" onclick={() => { void requestSnapshot(false); }} disabled={loading || refreshing}>{refreshing ? "Refreshing" : "Refresh"}</button>
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error" role="alert">{error}</Notice>{/if}
  {#if unavailableMessage}<Notice variant="info" role="status">{unavailableMessage}</Notice>{/if}
  {#if feedMessage}<Notice variant="warning" role="status">{feedMessage}</Notice>{/if}

  {#if loading}
    <LoadingState label="Loading event health" />
  {:else if unavailableMessage}
    <EmptyState title="Events is unavailable" description="Event publishing and consuming continue without the optional visibility service." />
  {:else}
    <MetricsLedger ariaLabel="Event health summary" items={ledgerItems} onSelect={handleLedgerSelect} />

    {#if diagnosticsError}
      <Notice variant="info" role="status">{diagnosticsError} Completeness of retained event history is unknown.</Notice>
    {:else if diagnostics?.gapDetected}
      <Notice variant="warning">Event history has a retention gap. Complete since {diagnostics.completeSince ? formatDate(diagnostics.completeSince) : "unknown"}; revision {diagnostics.revision.toLocaleString()}.</Notice>
    {/if}

    <div class="health-layout">
      <Panel eyebrow="Secondary" title="Consumer delivery health" class="consumer-health min-w-0">
        {#snippet actions()}<span class="text-sm text-base-content/70">{displayedConsumers.length} shown</span>{/snippet}
        <p class="text-sm text-base-content/70">Known Trellis consumers first; external consumers remain neutral.</p>
        {#if consumerError}
          <Notice variant="warning" role="status">
            {consumerError} Consumer delivery health is unavailable; the event rows below are unaffected.
          </Notice>
        {:else if displayedConsumers.length === 0}
          <EmptyState title="No consumers need attention" description="All known Trellis consumers are current or processing." />
        {:else}
          <DataTable>
            <thead><tr><th>Consumer deployment / contract</th><th>Status</th><th>Pending</th><th>Ack</th><th>Pulls</th><th>Oldest</th><th>Redelivered</th></tr></thead>
            <tbody>
              {#each displayedConsumers as consumer (`${consumer.stream}:${consumer.consumerName}`)}
                {@const selected = selectedConsumer?.row.consumerName === consumer.consumerName && selectedConsumer.row.stream === consumer.stream}
                <tr class:row-selected={selected}>
                  <td class="min-w-0">
                    <button type="button" class="link link-hover block max-w-xs truncate text-left trellis-identifier" onclick={() => { void inspectConsumer(consumer); }}>{consumer.deploymentId ?? consumer.consumerName}</button>
                    <span class="trellis-metadata trellis-identifier block max-w-xs truncate">{consumer.contractId ?? consumer.managedBy}{consumer.group ? ` / ${consumer.group}` : ""}</span>
                  </td>
                  <td><StatusBadge label={consumer.status} status={consumerVariant(consumer)} /></td>
                  <td class="tabular-nums" class:cell-pressure={consumer.pending > 0}>{consumer.pending.toLocaleString()}</td>
                  <td class="tabular-nums">{consumer.ackPending.toLocaleString()}</td>
                  <td class="tabular-nums">{consumer.waitingPulls.toLocaleString()}</td>
                  <td class="tabular-nums" class:cell-pressure={isAttentionConsumer(consumer)}>{ageLabel(consumer.oldestPendingAt)}</td>
                  <td class="tabular-nums" class:cell-pressure={(consumer.redelivered ?? 0) > 0}>{consumer.redelivered ?? 0}</td>
                </tr>
              {/each}
            </tbody>
          </DataTable>
        {/if}
      </Panel>

      <aside class="event-rail flex flex-col gap-4" aria-label="Event flow and integrity">
        <Panel eyebrow="Secondary" title="Event volume">
          <p class="text-sm text-base-content/70">{windowOption.label}</p>
          {#if !(metricsReady && metrics)}
            <Notice variant="warning" role="status">
              {visibleMetricsState === "pending" ? "Loading metrics for this window." : `${metricsError ?? "Event metrics are not permitted."} Event volume is unavailable for this window, not zero.`}
            </Notice>
          {:else}
            {#snippet actions()}<strong class="tabular-nums">{eventRate?.toLocaleString(undefined, { maximumFractionDigits: 1 })}/min</strong>{/snippet}
            <p class="text-sm text-base-content/70">{metrics?.summary.total.toLocaleString()} events · {windowOption.label}</p>
            <svg viewBox="0 0 260 52" preserveAspectRatio="none" role="img" aria-label={`Event volume over ${windowOption.label}`}>
              <path class="chart-grid" d="M0 46H260 M0 26H260" />
              <polyline class="event-line" points={eventPoints} />
            </svg>
          {/if}
        </Panel>
        <Panel eyebrow="Secondary" title="Integrity exceptions">
          {#if !(metricsReady && metrics)}
            <Notice variant="warning" role="status">
              {visibleMetricsState === "pending" ? "Loading metrics for this window." : `${metricsError ?? "Event metrics are not permitted."} Integrity exceptions are unavailable for this window, not zero.`}
            </Notice>
          {:else}
            {#snippet actions()}<strong class="tabular-nums text-error">{metrics?.summary.integrityExceptions}</strong>{/snippet}
            <p class="text-sm text-base-content/70">Verification and resolution failures</p>
            <svg viewBox="0 0 260 52" preserveAspectRatio="none" role="img" aria-label={`Integrity exceptions over ${windowOption.label}`}>
              <path class="chart-grid" d="M0 46H260 M0 26H260" />
              <polyline class="exception-line" points={exceptionPoints} />
            </svg>
            <div class="integrity-breakdown">
              {#each verificationIssues as status (status)}
                {#if verificationCount(status) > 0n}
                  <button aria-pressed={focus === status} onclick={() => selectFocus(status)}><span>{status.replaceAll("-", " ")}</span><strong>{verificationCount(status)}</strong></button>
                {/if}
              {/each}
              {#if (metrics?.summary.byResolution.malformed ?? 0) > 0}
                <button aria-pressed={focus === "malformed"} onclick={() => selectFocus("malformed")}><span>malformed</span><strong>{metrics?.summary.byResolution.malformed}</strong></button>
              {/if}
            </div>
          {/if}
        </Panel>
        <Panel eyebrow="Secondary" title="Highest-volume types">
          {#if !(metricsReady && metrics)}
            <Notice variant="warning" role="status">
              {visibleMetricsState === "pending" ? "Loading metrics for this window." : `${metricsError ?? "Event metrics are not permitted."} Event types by volume are unavailable for this window, not zero.`}
            </Notice>
          {:else}
            <div class="event-types">
              {#each eventTypesByCount.slice(0, 6) as eventType (`${eventType.ownerContractId}:${eventType.ownerEventName}`)}
                <button class:active={selectedEventType?.ownerContractId === eventType.ownerContractId && selectedEventType.ownerEventName === eventType.ownerEventName} aria-pressed={selectedEventType?.ownerContractId === eventType.ownerContractId && selectedEventType.ownerEventName === eventType.ownerEventName} onclick={() => selectEventType(eventType)}>
                  <span><strong>{eventType.ownerEventName}</strong><small>{eventType.ownerContractId}</small></span><b>{eventType.count.toLocaleString()}</b>
                   <i style={`--width: ${metrics?.summary.total ? boundedNumber(eventType.count) / boundedNumber(metrics.summary.total) * 100 : 0}%`}></i>
                </button>
              {:else}
                <p class="text-sm text-base-content/70">No resolved event types in this window.</p>
              {/each}
            </div>
          {/if}
        </Panel>
      </aside>
    </div>

    <Panel eyebrow="Primary" title="Consumer dead letters">
      {#if !selectedConsumer}
        <EmptyState title="Select a consumer" description="Choose a managed Consumer above to inspect its active dead letters." />
      {:else if deadLetterPanelError}
        <Notice variant="warning" role="status">
          {deadLetterPanelError} This consumer's dead letters could not be queried.
        </Notice>
      {:else if deadLetters.length === 0}
        <EmptyState title="No active dead letters" description="Exhausted managed Consumer deliveries appear here for replay or dismissal." />
      {:else}
        <DataTable>
          <thead><tr><th>Updated</th><th>Resource / dead letter</th><th>State</th><th>Deliveries</th><th>Error</th><th>Actions</th></tr></thead>
          <tbody>
            {#each deadLetters as deadLetter (deadLetter.deadLetterId)}
              <tr>
                <td>{formatDate(deadLetter.updatedAt)}</td>
                <td><button class="link link-hover trellis-identifier" onclick={() => inspectDeadLetter(deadLetter)}>{deadLetter.resourceId}</button><small class="block trellis-metadata trellis-identifier">{deadLetter.deadLetterId}</small></td>
                <td><StatusBadge label={deadLetter.state} status={deadLetter.state === "dead" ? "unhealthy" : "degraded"} /></td>
                <td>{deadLetter.deliveries.toLocaleString()}</td>
                <td>{deadLetterError[deadLetter.deadLetterId] || deadLetter.lastError || "-"}</td>
                <td class="space-x-2"><button class="btn btn-outline btn-xs" disabled={deadLetterBusy === deadLetter.deadLetterId} onclick={() => changeDeadLetter(deadLetter, "replay")}>Replay</button><button class="btn btn-error btn-outline btn-xs" disabled={deadLetterBusy === deadLetter.deadLetterId} onclick={() => changeDeadLetter(deadLetter, "dismiss")}>Dismiss</button></td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
      {/if}
      {#if selectedDeadLetter}
        <div class="payload-grid mt-4"><div><h3>Original event</h3><p class="trellis-identifier">{selectedDeadLetter.originalSubject}</p><pre>{jsonBlock(selectedDeadLetter.originalHeaders)}</pre></div><div><h3>Original payload bytes</h3><pre>{Array.from(selectedDeadLetter.originalPayload, (byte) => byte.toString(16).padStart(2, "0")).join(" ")}</pre></div></div>
      {/if}
    </Panel>

    <Panel eyebrow="Primary" title={focusTitle}>
      {#snippet actions()}
        <div class="flex items-center gap-2">
          <input class="input input-bordered input-sm w-56" placeholder="Search event metadata" bind:value={searchText} onchange={resetAndLoad} />
          {#if selectedEventType || ownerContractId || publisherDeploymentId || searchText}<button class="btn btn-ghost btn-sm" onclick={clearEventFilters}>Clear scope</button>{/if}
           <span class="text-sm text-base-content/70">{rows.length} shown</span>
        </div>
      {/snippet}
      <p class="text-sm text-base-content/70">{focusDescription}</p>
      {#if selectedEventType || ownerContractId || publisherDeploymentId}
        <div class="active-scope" aria-label="Active event scope">
          {#if selectedEventType}<button onclick={() => { selectedEventType = null; resetAndLoad(); }}>type: {selectedEventType.ownerContractId} / {selectedEventType.ownerEventName} ×</button>{/if}
          {#if ownerContractId}<button onclick={() => { ownerContractId = ''; resetAndLoad(); }}>owner: {ownerContractId} ×</button>{/if}
          {#if publisherDeploymentId}<button onclick={() => { publisherDeploymentId = ''; resetAndLoad(); }}>publisher: {publisherDeploymentId} ×</button>{/if}
        </div>
      {/if}
      {#if rows.length === 0}
        <EmptyState title="No events match this operational view" description="Choose another status, widen the window, or clear the active scope." />
      {:else}
        <DataTable>
          <thead><tr><th>Time</th><th>Subject / event</th><th>Owner</th><th>Publisher</th><th>Integrity</th><th>Payload</th></tr></thead>
          <tbody>
            {#each rows as row (`${row.streamSequence}:${row.eventId}`)}
              <tr>
                <td class="whitespace-nowrap">{formatDate(row.eventTime)}</td>
                <td class="min-w-0">
                  <button type="button" class="link link-hover block max-w-md truncate text-left trellis-identifier" onclick={() => { void inspectEvent(row); }}>{row.subject}</button>
                  <span class="trellis-metadata">seq {row.streamSequence}</span>
                </td>
                <td class="min-w-0"><button type="button" class="link link-hover block max-w-xs truncate text-left trellis-identifier" onclick={() => selectOwner(row.ownerContractId)}>{subjectOwner(row)}</button></td>
                <td><button type="button" class="link link-hover trellis-identifier" onclick={() => selectPublisher(row.publisherDeploymentId)}>{publisherLabel(row)}</button></td>
                <td><span class="flex gap-1"><span class={["badge badge-sm trellis-badge-soft border-0", statusBadgeClass(row.verificationStatus)]}>{row.verificationStatus}</span>{#if row.resolution !== "resolved"}<span class={["badge badge-sm trellis-badge-soft border-0", statusBadgeClass(row.resolution)]}>{row.resolution}</span>{/if}</span></td>
                 <td class="tabular-nums">{formatBytes(boundedNumber(row.payloadSizeBytes))}</td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
        {#if cursorBackStack.length > 0 || nextCursor}
          <div class="flex items-center justify-end gap-3 text-sm text-base-content/70">
            <button class="btn btn-outline btn-xs" onclick={goPrevious} disabled={cursorBackStack.length === 0}>Previous</button>
            <span>Page {cursorBackStack.length + 1}</span>
            <button class="btn btn-outline btn-xs" onclick={goNext} disabled={!nextCursor}>Next</button>
          </div>
        {/if}
      {/if}
    </Panel>

    {#if detailLoading}
      <Panel eyebrow="Detail" title="Loading detail"><LoadingState label="Loading detail" /></Panel>
    {:else if selectedEvent}
      <Panel eyebrow="Detail" title="Event inspect">
        {#if detailError}<Notice variant="warning" role="status">{detailError}</Notice>{/if}
        <div class="detail-grid">
          <div><h3>Event identity</h3><p><span>id</span><code>{selectedEvent.event.eventId}</code></p><p><span>time</span><code>{selectedEvent.event.eventTime}</code></p><p><span>subject</span><code>{selectedEvent.event.subject}</code></p><p><span>stream sequence</span><code>{selectedEvent.event.streamSequence}</code></p></div>
          <div><h3>Owner from subject/catalog</h3><p><span>contract</span><code>{selectedEvent.event.ownerContractId ?? "unresolved"}</code></p><p><span>event</span><code>{selectedEvent.event.ownerEventName ?? "-"}</code></p><p><span>resolution</span><code>{selectedEvent.event.resolution}</code></p></div>
           <div><h3>Publisher from verified session</h3><p><span>status</span><code>{selectedEvent.event.verificationStatus}</code></p><p><span>deployment</span><code>{selectedEvent.event.publisherDeploymentId ?? "-"}</code></p><p><span>instance</span><code>{selectedEvent.event.publisherInstanceId ?? "-"}</code></p><p><span>participant</span><code>{selectedEvent.event.publisherParticipantId ?? "-"}</code></p><p><span>digest</span><code>{selectedEvent.event.publisherParticipantDigest ?? "-"}</code></p></div>
           <div><h3>Matching consumers</h3><p><span>durables</span><code>{matchingConsumers.map((consumer) => consumer.consumerName).join(", ") || "-"}</code></p></div>
        </div>
        <div class="payload-grid"><div><h3>Headers</h3><pre>{jsonBlock(selectedEvent.headers)}</pre></div><div><h3>Payload</h3>{#if selectedEvent.decodeError}<p class="text-xs text-error">{selectedEvent.decodeError}</p>{/if}<pre>{selectedEvent.payloadText ?? jsonBlock(selectedEvent.payload)}</pre></div></div>
      </Panel>
    {:else if selectedConsumer}
      <Panel eyebrow="Consumer" title={selectedConsumer.row.deploymentId ?? selectedConsumer.row.consumerName}>
        {#if detailError}<Notice variant="warning" role="status">{detailError}</Notice>{/if}
        <div class="detail-grid">
          <div><h3>Ownership</h3><p><span>managed by</span><code>{selectedConsumer.row.managedBy}</code></p><p><span>deployment</span><code>{selectedConsumer.row.deploymentId ?? "-"}</code></p><p><span>contract</span><code>{selectedConsumer.row.contractId ?? "-"}</code></p><p><span>group</span><code>{selectedConsumer.row.group ?? "-"}</code></p></div>
          <div><h3>Live state</h3><p><span>status</span><code>{selectedConsumer.row.status}</code></p><p><span>pending</span><code>{selectedConsumer.row.pending}</code></p><p><span>ack pending</span><code>{selectedConsumer.row.ackPending}</code></p><p><span>waiting pulls</span><code>{selectedConsumer.row.waitingPulls}</code></p></div>
        </div>
        <pre>{jsonBlock(selectedConsumer.detail)}</pre>
      </Panel>
    {/if}
  {/if}
  <ConfirmationModal bind:this={confirmationModal} />
</section>

<style>
  .events-page { display: grid; gap: 1rem; }
  .health-layout { display: grid; gap: 1.15rem; grid-template-columns: minmax(0, 1fr) 20rem; align-items: start; }
  .event-rail { min-width: 0; }
  .row-selected { background: color-mix(in oklab, var(--color-primary) 10%, var(--color-base-100)); }
  .cell-pressure { color: var(--color-error); font-weight: 700; }
  .event-rail svg { display: block; height: 3.5rem; width: 100%; }
  .chart-grid { fill: none; stroke: color-mix(in oklab, var(--color-base-300) 75%, transparent); stroke-width: 1; }
  .event-line, .exception-line { fill: none; stroke-linecap: round; stroke-linejoin: round; stroke-width: 2; }
  .event-line { stroke: var(--color-info); } .exception-line { stroke: var(--color-error); }
  .integrity-breakdown { display: grid; gap: 0.25rem; }
  .integrity-breakdown button { background: transparent; border: 0; cursor: pointer; display: flex; font-size: 0.8rem; justify-content: space-between; padding: 0.2rem 0; text-align: left; text-transform: capitalize; }
  .integrity-breakdown button:hover span { text-decoration: underline; }
  .event-types { display: grid; gap: 0.35rem; min-width: 0; }
  .event-types button { background: transparent; border: 0; border-radius: 0.35rem; cursor: pointer; display: grid; gap: 0.15rem 0.5rem; grid-template-columns: minmax(0, 1fr) auto; margin: 0 -0.25rem; padding: 0.3rem 0.25rem; text-align: left; width: calc(100% + 0.5rem); }
  .event-types button:hover, .event-types button.active { background: color-mix(in oklab, var(--color-base-200) 72%, transparent); }
  .event-types button span { min-width: 0; }
  .event-types button strong, .event-types button small { display: block; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .event-types button strong { font-size: 0.8rem; } .event-types button small { color: color-mix(in oklab, var(--color-base-content) 62%, transparent); font-size: 0.72rem; }
  .event-types button b { font-size: 0.8rem; font-variant-numeric: tabular-nums; }
  .event-types button i { background: color-mix(in oklab, var(--color-info) 70%, var(--color-base-300)); border-radius: 0.2rem; grid-column: 1 / -1; height: 0.2rem; width: var(--width); }
  .active-scope { display: flex; flex-wrap: wrap; gap: 0.35rem; }
  .active-scope button { background: color-mix(in oklab, var(--color-base-200) 82%, transparent); border: 1px solid var(--color-base-300); border-radius: 999px; cursor: pointer; font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace; font-size: 0.72rem; padding: 0.25rem 0.6rem; }
  .active-scope button:hover { background: color-mix(in oklab, var(--color-base-200) 60%, transparent); }
  .detail-grid, .payload-grid { display: grid; gap: 0.75rem; grid-template-columns: repeat(auto-fit, minmax(16rem, 1fr)); }
  .detail-grid h3 { font-size: 0.78rem; font-weight: 700; letter-spacing: 0.08em; margin-bottom: 0.35rem; text-transform: uppercase; }
  .detail-grid p { align-items: baseline; display: flex; gap: 0.45rem; margin: 0.15rem 0; }
  .detail-grid p span { color: color-mix(in oklab, var(--color-base-content) 64%, transparent); font-size: 0.78rem; min-width: 6rem; }
  code, pre { font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace; }
  pre { background: color-mix(in oklab, var(--color-base-200) 80%, transparent); border: 1px solid var(--color-base-300); border-radius: var(--radius-box, 1rem); max-height: 24rem; overflow: auto; padding: 0.75rem; white-space: pre-wrap; }

  @media (max-width: 75rem) {
    .health-layout { grid-template-columns: 1fr; }
    .event-rail { display: grid; grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr)); }
  }
</style>
