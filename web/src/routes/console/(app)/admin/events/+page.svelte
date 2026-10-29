<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { afterNavigate } from "$app/navigation";
  import { page } from "$app/state";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import ColumnChart from "$lib/components/ColumnChart.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataPanel from "$lib/components/DataPanel.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import MetricsLedger from "$lib/components/MetricsLedger.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { boundedNumber, errorMessage, formatDate } from "$lib/format";
  import { getConnection, getTrellis } from "$lib/trellis";
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
  import { decodeKnownJsonBytes } from "$lib/console/display_value.ts";
  import {
    apiLabel,
    deliveryStatus,
    errorText,
    eventCheckLabel,
    friendlyDuration,
    sentenceName,
  } from "$lib/console/plain_labels.ts";
  import { createServiceNames } from "$lib/console/service_names.svelte.ts";
  import { type apis } from "trellis-web-generated";
  import { subjectMatches } from "./subject";

  const names = createServiceNames();

  type WindowValue = "15m" | "1h" | "6h" | "24h" | "7d";
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
  type DeadLetter = apis.events.DeadLettersQueryOutput["items"][number];
  type DeadLetterDetail = apis.events.DeadLettersInspectOutput["deadLetter"];

  /** An event's data, decoded for display when it is JSON or text. */
  type EventData = { kind: "value"; value: unknown } | { kind: "text"; text: string } | { kind: "bytes"; size: number };

  type EventInspect = {
    event: EventRow;
    data?: EventData;
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
    oldestPendingAt?: string;
  };

  const trellis = getTrellis();
  const connection = getConnection();
  const rpcTimeout = 10_000;
  const pageLimit = 40;
  const windows: Array<{ value: WindowValue; label: string; title: string; minutes: number }> = [
    { value: "15m", label: "15m", title: "Last 15 minutes", minutes: 15 },
    { value: "1h", label: "1h", title: "Last hour", minutes: 60 },
    { value: "6h", label: "6h", title: "Last 6 hours", minutes: 360 },
    { value: "24h", label: "24h", title: "Last 24 hours", minutes: 1_440 },
    { value: "7d", label: "7d", title: "Last 7 days", minutes: 10_080 },
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
  const listTabs: Array<{ value: Focus; label: string }> = [
    { value: "all", label: "All" },
    { value: "exceptions", label: "Problems" },
    { value: "largest", label: "Largest" },
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
  let deadLetters = $state.raw<DeadLetter[]>([]);
  let openDeadLetter = $state<string | null>(null);
  let deadLetterDetail = $state.raw<DeadLetterDetail | null>(null);
  let deadLetterError = $state<Record<string, string>>({});
  let deadLetterBusy = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  let selectedEvent = $state.raw<EventInspect | null>(null);
  /** The event row whose details are open beneath it. */
  let openEventKey = $state<string | null>(null);
  let selectedConsumer = $state.raw<ConsumerRow | null>(null);
  let detailLoading = $state(false);
  let detailError = $state<string | null>(null);
  let focus = $state<Focus>(asEventFocus(page.url.searchParams.get("focus")) ?? "all");
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
  const eventTypesByCount = $derived.by(() => [...(metrics?.summary.eventTypes ?? [])].sort((a, b) => boundedNumber(b.count - a.count)));
  const matchingConsumers = $derived.by(() => {
    const subject = selectedEvent?.event.subject;
    return subject
      ? consumers.filter((consumer) => consumer.filterSubjects.some((filter) => subjectMatches(filter, subject)))
      : [];
  });
  const sortedConsumers = $derived.by(() =>
    [...consumers].sort((a, b) =>
      consumerSeverityOf(a.status) - consumerSeverityOf(b.status) ||
      consumerName(a).localeCompare(consumerName(b))
    )
  );
  const attentionConsumers = $derived(sortedConsumers.filter(isAttentionConsumer));
  const displayedConsumers = $derived(attentionConsumersOnly ? attentionConsumers : sortedConsumers);
  const consumersByResource = $derived(new Map(consumers.map((consumer) => [consumer.resourceId, consumer])));
  const failedByResource = $derived.by(() => {
    const counts = new Map<string, number>();
    for (const item of deadLetters) {
      if (item.state === "dead") counts.set(item.resourceId, (counts.get(item.resourceId) ?? 0) + 1);
    }
    return counts;
  });
  const shownDeadLetters = $derived(
    selectedConsumer ? deadLetters.filter((item) => item.resourceId === selectedConsumer?.resourceId) : deadLetters,
  );
  const failedDeliveries = $derived(metricsReady && metrics ? metrics.summary.deadLettersByState.dead : undefined);
  const chartPoints = $derived(
    metricsReady && metrics
      ? metrics.buckets.map((bucket) => ({ ...bucketLabel(bucket.start), value: boundedNumber(bucket.total) }))
      : [],
  );
  const problemCounts = $derived.by(() => {
    if (!metricsReady || !metrics) return [];
    const counts: Array<{ focus: Focus; label: string; count: bigint }> = verificationIssues
      .map((status) => ({ focus: status as Focus, label: eventCheckLabel(status, "resolved").label, count: verificationCount(status) }));
    counts.push({ focus: "unresolved", label: "Unknown event type", count: metrics.summary.byResolution.unresolved ?? 0n });
    counts.push({ focus: "malformed", label: "Unreadable", count: metrics.summary.byResolution.malformed ?? 0n });
    return counts.filter((item) => item.count > 0n);
  });

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

  function isVerificationStatus(value: unknown): value is EventVerificationStatus {
    return value === "verified" || verificationIssues.includes(value as EventVerificationStatus);
  }

  function isConsumerManagedBy(value: unknown): value is ConsumerManagedBy {
    return value === "authority" || value === "platform" || value === "external";
  }

  function toConsumerRow(value: unknown): ConsumerRow | null {
    const row = objectRecord(value);
    const stream = stringValue(row.stream);
    const name = stringValue(row.consumerName);
    if (!stream || !name) return null;
    const managedBy = isConsumerManagedBy(row.managedBy) ? row.managedBy : stringValue(row.deploymentId) ? "authority" : "external";
    return {
      resourceId: stringValue(row.resourceId) ?? "",
      deploymentId: stringValue(row.deploymentId),
      contractId: stringValue(row.contractId),
      group: stringValue(row.group),
      stream,
      consumerName: name,
      filterSubjects: stringArray(row.filterSubjects),
      managedBy,
      status: managedBy === "external" ? "unmanaged" : consumerStatus(row.status),
      pending: numberValue(row.pending) ?? 0,
      ackPending: numberValue(row.ackPending) ?? 0,
      waitingPulls: numberValue(row.waitingPulls) ?? 0,
      redelivered: numberValue(row.redelivered),
      oldestPendingAt: stringValue(row.oldestPendingAt),
    };
  }

  /** Event data is shown as fields when it is JSON, as text, or by size. */
  function decodeData(payload: Uint8Array | undefined): EventData | undefined {
    if (!payload) return undefined;
    const decoded = decodeKnownJsonBytes(payload);
    if (decoded.ok) return { kind: "value", value: decoded.value };
    try {
      return { kind: "text", text: new TextDecoder("utf-8", { fatal: true }).decode(payload) };
    } catch {
      return { kind: "bytes", size: payload.byteLength };
    }
  }

  function isAttentionConsumer(consumer: ConsumerRow): boolean {
    return deliveryStatus(consumer.status).attention;
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

  /** The service a delivery goes to, by name wherever Trellis knows it. */
  function consumerName(consumer: ConsumerRow): string {
    const named = names.deployment(consumer.deploymentId);
    if (named) return named;
    if (consumer.managedBy === "platform" && consumer.contractId) return `Trellis ${apiLabel(consumer.contractId).toLowerCase()}`;
    return sentenceName(consumer.consumerName);
  }

  /** One short line saying how a service's deliveries stand. */
  function consumerSummary(consumer: ConsumerRow): string {
    const parts: string[] = [];
    const failed = failedByResource.get(consumer.resourceId) ?? 0;
    if (failed > 0) parts.push(`${failed} failed`);
    if (consumer.pending > 0) parts.push(`${consumer.pending.toLocaleString()} waiting`);
    if (consumer.oldestPendingAt) parts.push(`oldest ${ageLabel(consumer.oldestPendingAt)}`);
    if (parts.length === 0) return consumer.ackPending > 0 ? `${consumer.ackPending} in progress` : "Nothing waiting";
    return parts.join(" · ");
  }

  function deliveryExplanation(consumer: ConsumerRow): string {
    switch (consumer.status) {
      case "current": return "Has every event it listens for.";
      case "processing": return "Handling events right now; nothing is waiting.";
      case "behind": return "Events are waiting for this service to pick them up.";
      case "saturated": return "Handling as many events as it is allowed to at once; more are waiting.";
      case "failing": return "Some events are being delivered again after the service didn't finish them.";
      case "inactive": return "Events are waiting and nothing is picking them up. Check that the service is running.";
      case "missing": return "This service should receive events but its subscription doesn't exist.";
      case "orphaned": return "A subscription remains for a service that no longer exists.";
      case "unmanaged": return "Set up outside Trellis.";
      default: return "";
    }
  }

  const ledgerItems = $derived(projectEventsHealthLedger({
    metricsState: visibleMetricsState,
    consumersState,
    total: metricsReady && metrics ? metrics.summary.total : undefined,
    integrityExceptions: metricsReady && metrics ? metrics.summary.integrityExceptions : undefined,
    failedDeliveries,
    windowMinutes: windowOption.minutes,
    attentionConsumers: attentionConsumers.length,
    shownConsumers: consumers.length,
    focus,
    attentionConsumersOnly,
    failedActive: false,
  }));

  function handleLedgerSelect(id: string) {
    if (id === "consumers") {
      attentionConsumersOnly = !attentionConsumersOnly;
      document.querySelector(".consumer-health")?.scrollIntoView({ behavior: "smooth", block: "start" });
    } else if (id === "failed") {
      selectConsumer(null);
      document.getElementById("failed-deliveries")?.scrollIntoView({ behavior: "smooth", block: "start" });
    } else if (id === "all" || id === "exceptions") {
      selectFocus(id);
      document.getElementById("recent-events")?.scrollIntoView({ behavior: "smooth", block: "start" });
    }
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
    return Number.isNaN(time) ? value : friendlyDuration(Date.now() - time);
  }

  function bucketLabel(iso: string): { label: string; tick: string } {
    const date = new Date(iso);
    return {
      label: date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }),
      tick: windowValue === "7d"
        ? date.toLocaleDateString(undefined, { weekday: "short" })
        : date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" }),
    };
  }

  /** "events.v1.<api>.Connections.Opened" → "Connections.Opened". */
  function subjectEventName(subject: string): string {
    const parts = subject.split(".");
    return parts[0] === "events" && parts.length > 3 ? parts.slice(3).join(".") : (parts.at(-1) ?? subject);
  }

  function eventLabel(row: { ownerEventName?: string; subject: string }): string {
    return sentenceName(row.ownerEventName ?? subjectEventName(row.subject));
  }

  function publisherLabel(row: EventRow): string {
    const named = names.deployment(row.publisherDeploymentId);
    if (named) return named;
    if (row.publisherKind === "user") return "A person";
    if (row.publisherKind === "device") return "A device";
    if (row.publisherKind === "service") return "A service";
    return "Unknown sender";
  }

  function focusTitle(value: Focus): string {
    if (value === "exceptions") return "Events that couldn't be verified";
    if (value === "all") return "Recent events";
    if (value === "largest") return "Largest events";
    if (value === "unresolved") return "Events of an unknown type";
    if (value === "malformed") return "Unreadable events";
    return `Events: ${eventCheckLabel(value, "resolved").label.toLowerCase()}`;
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

  async function loadConsumers(): Promise<apis.events.ConsumersQueryOutput["items"]> {
    const items: apis.events.ConsumersQueryOutput["items"] = [];
    for await (const item of trellis.consumersQuery.items({ page: { limit: 500 } }, { timeout: rpcTimeout })) {
      items.push(item.orThrow());
    }
    return items;
  }

  /** Every delivery still awaiting a decision, across all services. */
  async function loadDeadLetters(): Promise<DeadLetter[]> {
    const items: DeadLetter[] = [];
    for await (const item of trellis.deadLettersQuery.items({ state: ["dead", "replayPending", "replaying"], page: { limit: 500 } }, { timeout: rpcTimeout })) {
      items.push(item.orThrow());
    }
    return items;
  }

  function unavailableText(loadError: unknown): string | null {
    const message = errorMessage(loadError);
    const normalized = message.toLowerCase();
    if (normalized.includes("no responders") || normalized.includes("not currently reachable") || normalized.includes("inactive contract")) {
      return "The events service isn't running, so event history and failed deliveries can't be shown.";
    }
    if (normalized.includes("permissions violation")) {
      return "You haven't allowed the Console to see events. Use Change access on your account to allow it.";
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
    const requestedWindow = capturedQuery.window;
    if (showLoading) loading = true;
    else refreshing = true;
    // The event query is primary. Delivery health, metrics, diagnostics, and
    // failed deliveries are independent: a denied or failed optional read must
    // not blank permitted event rows.
    const [eventResult, consumerResult, metricsResult, diagnosticsResult, deadLetterResult] =
      await Promise.allSettled([
        trellis.eventsQuery(capturedQuery.eventQuery, { timeout: rpcTimeout }).orThrow(),
        loadConsumers(),
        trellis.eventsMetrics(capturedQuery.metricsInput, { timeout: rpcTimeout }).orThrow(),
        trellis.diagnostics({}, { timeout: rpcTimeout }).orThrow(),
        loadDeadLetters(),
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
        if (unavailable) unavailableMessage = unavailable;
        else error = errorMessage(eventResult.reason);
        rows = [];
        nextCursor = undefined;
      }
      if (consumerResult.status === "fulfilled") {
        consumers = consumerResult.value
          .map(toConsumerRow)
          .filter((row): row is ConsumerRow => row !== null);
        consumersState = "ready";
        consumerError = null;
      } else {
        consumers = [];
        consumersState = "unavailable";
        consumerError = errorMessage(consumerResult.reason);
      }
      if (metricsResult.status === "fulfilled") {
        metrics = metricsResult.value;
        metricsState = "ready";
        metricsScope = requestedWindow;
        metricsError = null;
      } else {
        metrics = null;
        metricsState = "unavailable";
        metricsScope = requestedWindow;
        metricsError = errorMessage(metricsResult.reason);
      }
      if (diagnosticsResult.status === "fulfilled") {
        diagnostics = diagnosticsResult.value;
        diagnosticsError = null;
      } else {
        diagnostics = null;
        diagnosticsError = errorMessage(diagnosticsResult.reason);
      }
      if (deadLetterResult.status === "fulfilled") {
        deadLetters = deadLetterResult.value;
        deadLetterPanelError = null;
      } else {
        deadLetters = [];
        deadLetterPanelError = errorMessage(deadLetterResult.reason);
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
    openEventKey = null;
    selectedConsumer = null;
    openDeadLetter = null;
    deadLetterDetail = null;
    detailError = null;
    detailLoading = false;
    deadLetterError = {};
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
    // Metrics rows carry bigint counts; the filter keeps only the identity.
    const { ownerContractId, ownerEventName } = eventType;
    selectedEventType = selectedEventType?.ownerContractId === ownerContractId && selectedEventType.ownerEventName === ownerEventName ? null : { ownerContractId, ownerEventName };
    if (selectedEventType && focus === "exceptions") focus = "all";
    resetAndLoad();
  }

  function clearEventFilters() {
    searchText = "";
    ownerContractId = "";
    publisherDeploymentId = "";
    selectedEventType = null;
    resetAndLoad();
  }

  function selectPublisher(deployment: string | undefined) {
    if (!deployment) return;
    publisherDeploymentId = deployment;
    if (focus === "exceptions") focus = "all";
    resetAndLoad();
  }

  function eventKey(row: EventRow): string {
    return row.eventId ?? String(row.streamSequence);
  }

  /** Opens a row's details beneath it, or closes them when already open. */
  async function toggleEvent(row: EventRow) {
    if (openEventKey === eventKey(row)) {
      detailOwnership.invalidate();
      openEventKey = null;
      selectedEvent = null;
      detailLoading = false;
      return;
    }
    openEventKey = eventKey(row);
    await inspectEvent(row);
  }

  async function inspectEvent(row: EventRow) {
    const token = detailOwnership.begin("event", row.eventId ?? String(row.streamSequence));
    ++deadLetterSequence;
    // Selecting B clears A's visible detail synchronously.
    detailLoading = true;
    detailError = null;
    selectedEvent = null;
    try {
      const input: apis.events.InspectInput = row.eventId ? { eventId: row.eventId } : { streamSequence: row.streamSequence };
      const detail = await trellis.eventsInspect(input, { timeout: rpcTimeout }).orThrow();
      if (!detailOwnership.owns(token) || disposed) return;
      selectedEvent = { event: detail.event.row, data: decodeData(detail.event.payload) };
    } catch (inspectError) {
      if (!detailOwnership.owns(token) || disposed) return;
      detailError = errorMessage(inspectError);
      selectedEvent = { event: row };
    } finally {
      if (detailOwnership.owns(token) && !disposed) detailLoading = false;
    }
  }

  /** Selecting a service narrows the failed deliveries to it. */
  function selectConsumer(row: ConsumerRow | null) {
    selectionEpoch += 1;
    ++deadLetterSequence;
    openDeadLetter = null;
    deadLetterDetail = null;
    if (!row || selectedConsumer?.resourceId === row.resourceId) {
      selectedConsumer = null;
      return;
    }
    detailOwnership.begin("consumer", row.consumerName);
    selectedConsumer = row;
  }

  async function toggleDeadLetter(deadLetter: DeadLetter) {
    if (openDeadLetter === deadLetter.deadLetterId) {
      openDeadLetter = null;
      deadLetterDetail = null;
      return;
    }
    const sequence = ++deadLetterSequence;
    openDeadLetter = deadLetter.deadLetterId;
    deadLetterDetail = null;
    deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: "" };
    try {
      const result = await trellis.deadLettersInspect({ resourceId: deadLetter.resourceId, deadLetterId: deadLetter.deadLetterId }, { timeout: rpcTimeout }).orThrow();
      if (sequence === deadLetterSequence && !disposed && openDeadLetter === deadLetter.deadLetterId) deadLetterDetail = result.deadLetter;
    } catch (cause) {
      if (sequence === deadLetterSequence && !disposed) deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: errorMessage(cause) };
    }
  }

  function deadLetterService(deadLetter: DeadLetter): string {
    const consumer = consumersByResource.get(deadLetter.resourceId);
    return consumer ? consumerName(consumer) : "A service";
  }

  async function changeDeadLetter(deadLetter: DeadLetter, action: "replay" | "dismiss") {
    const capturedQueryEpoch = queryEpoch;
    const capturedKey = desiredQuery.key;
    const capturedSelectionEpoch = selectionEpoch;
    const owned = () =>
      !disposed &&
      capturedQueryEpoch === queryEpoch &&
      capturedKey === desiredQuery.key &&
      capturedSelectionEpoch === selectionEpoch;
    const input = { resourceId: deadLetter.resourceId, deadLetterId: deadLetter.deadLetterId, expectedRevision: deadLetter.revision, requestId: crypto.randomUUID() };
    const service = deadLetterService(deadLetter);
    const confirmed = await confirmationModal?.confirm({
      title: action === "replay" ? "Retry this delivery?" : "Dismiss this failed delivery?",
      message: action === "replay"
        ? `The event is sent to ${service} again.`
        : `${service} won't receive this event. This can't be undone.`,
      confirmLabel: action === "replay" ? "Retry" : "Dismiss",
    });
    if (!confirmed || !owned() ||
      !deadLetters.some((item) => item.deadLetterId === input.deadLetterId && item.revision === input.expectedRevision)) return;
    const mutationToken = ++deadLetterBusyGeneration;
    deadLetterBusy = input.deadLetterId;
    deadLetterError = { ...deadLetterError, [deadLetter.deadLetterId]: "" };
    try {
      await runDeadLetterMutation({
        mounted: owned,
        mutate: async () => {
          if (action === "replay") await trellis.deadLettersReplay(input, { timeout: rpcTimeout }).orThrow();
          else await trellis.deadLettersDismiss(input, { timeout: rpcTimeout }).orThrow();
        },
        followUp: async () => {
          if (!owned()) return;
          return requestSnapshot(false);
        },
        onError: (cause) => {
          if (!owned()) return;
          deadLetterError = {
            ...deadLetterError,
            [deadLetter.deadLetterId]: classifyMutationError(cause).kind === "unknown"
              ? "Outcome unknown. Refresh and check this delivery before trying again."
              : errorMessage(cause),
          };
        },
      });
    } finally {
      if (!disposed && deadLetterBusyGeneration === mutationToken && deadLetterBusy === input.deadLetterId) deadLetterBusy = null;
    }
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
        feedOnline = status === "live";
        feedMessage = status === "reconnecting"
          ? `Live updates paused; trying again in ${((detail?.retryInMs ?? 1_000) / 1_000).toFixed(0)}s. Refresh still works.`
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

{#snippet eventDetail(detail: EventInspect)}
  {@const event = detail.event}
  {@const check = eventCheckLabel(event.verificationStatus, event.resolution)}
  <div class="event-detail">
    {#if detailError}<Notice variant="warning" role="status">Some details couldn't be loaded: {detailError}</Notice>{/if}
    <div class="space-y-4">
      <dl class="fact-list">
        <div><dt>Published</dt><dd>{formatDate(event.eventTime)}</dd></div>
        <div><dt>From</dt><dd>{publisherLabel(event)}</dd></div>
        <div><dt>Check</dt><dd><StatusBadge label={check.label} status={check.ok ? "healthy" : "degraded"} /></dd></div>
        <div><dt>Delivered to</dt><dd>{matchingConsumers.map(consumerName).join(", ") || "No service listens for it"}</dd></div>
      </dl>
      <div class="min-w-0">
        {#if detail.data?.kind === "value"}<DataPanel value={detail.data.value} />
        {:else}
          <h3 class="text-sm font-semibold">Data</h3>
          <div class="mt-2">
            {#if !detail.data}<p class="text-sm text-base-content/60">No data was loaded.</p>
            {:else if detail.data.kind === "text"}<p class="text-sm whitespace-pre-wrap">{detail.data.text}</p>
            {:else}<p class="text-sm text-base-content/60">This event's data isn't readable text ({formatBytes(detail.data.size)}).</p>{/if}
          </div>
        {/if}
      </div>
    </div>
  </div>
{/snippet}

<svelte:document onvisibilitychange={handleVisibilityChange} />
<svelte:head><title>Events · Trellis Console</title></svelte:head>

<section class="events-page">
  <PageToolbar title="Events" description="What your services publish, and whether it reached the services listening for it.">
    {#snippet meta()}
      <span class={["badge badge-sm", feedOnline ? "badge-success" : "badge-neutral"]}>{feedOnline ? "Live" : "Not live"}</span>
      {#if lastUpdated}<span class="text-xs text-base-content/55">Updated {lastUpdated.toLocaleTimeString()}</span>{/if}
    {/snippet}
    {#snippet actions()}
      <div class="trellis-segment" role="group" aria-label="Time range">
        {#each windows as option (option.value)}
          <button type="button" title={option.title} class:active={windowValue === option.value} aria-pressed={windowValue === option.value} onclick={() => { windowValue = option.value; resetAndLoad(); }}>{option.label}</button>
        {/each}
      </div>
      <button class="btn btn-outline btn-sm" onclick={() => { void requestSnapshot(false); }} disabled={loading || refreshing}>{refreshing ? "Refreshing" : "Refresh"}</button>
    {/snippet}
  </PageToolbar>

  {#if error}<Notice variant="error" role="alert">{error}</Notice>{/if}
  {#if feedMessage}<Notice variant="warning" role="status">{feedMessage}</Notice>{/if}

  {#if loading}
    <LoadingState label="Loading events" />
  {:else if unavailableMessage}
    <EmptyState title="Events can't be shown" description={unavailableMessage} />
  {:else}
    <MetricsLedger ariaLabel="Event health summary" items={ledgerItems} onSelect={handleLedgerSelect} />

    {#if diagnostics?.gapDetected}
      <Notice variant="warning">Some older event history is missing. History is complete from {diagnostics.completeSince ? formatDate(diagnostics.completeSince) : "an unknown time"}.</Notice>
    {:else if diagnosticsError}
      <Notice variant="info" role="status">It isn't known whether event history is complete: {diagnosticsError}</Notice>
    {/if}

    <div id="recent-events">
      <Panel title={focusTitle(focus)}>
        {#snippet actions()}
          <div class="flex flex-wrap items-center gap-2">
            <div class="trellis-segment" role="group" aria-label="Which events">
              {#each listTabs as tab (tab.value)}
                <button type="button" class:active={focus === tab.value} aria-pressed={focus === tab.value} onclick={() => selectFocus(tab.value)}>{tab.label}</button>
              {/each}
            </div>
            <label class="input input-sm w-56">
              <span class="sr-only">Search events</span>
              <input type="search" placeholder="Search events" bind:value={searchText} onchange={resetAndLoad} />
            </label>
          </div>
        {/snippet}
        {#if focus !== "all" && focus !== "largest" && problemCounts.length > 0}
          <div class="chip-row" aria-label="Kinds of problem">
            {#each problemCounts as problem (problem.focus)}
              <button type="button" class="trellis-chip" class:active={focus === problem.focus} aria-pressed={focus === problem.focus} onclick={() => selectFocus(focus === problem.focus ? "exceptions" : problem.focus)}>{problem.label} <b>{problem.count.toLocaleString()}</b></button>
            {/each}
          </div>
        {/if}
        {#if selectedEventType || ownerContractId || publisherDeploymentId || searchText}
          <div class="chip-row" aria-label="Active filter">
            {#if selectedEventType}<button type="button" class="trellis-chip active" onclick={() => { selectedEventType = null; resetAndLoad(); }}>{sentenceName(selectedEventType.ownerEventName)} <span aria-hidden="true">×</span></button>{/if}
            {#if ownerContractId}<button type="button" class="trellis-chip active" onclick={() => { ownerContractId = ""; resetAndLoad(); }}>{apiLabel(ownerContractId)} <span aria-hidden="true">×</span></button>{/if}
            {#if publisherDeploymentId}<button type="button" class="trellis-chip active" onclick={() => { publisherDeploymentId = ""; resetAndLoad(); }}>From {names.deployment(publisherDeploymentId) ?? "one service"} <span aria-hidden="true">×</span></button>{/if}
            <button type="button" class="btn btn-ghost btn-xs" onclick={clearEventFilters}>Clear filter</button>
          </div>
        {/if}
        {#if rows.length === 0}
          <EmptyState
            title={focus === "exceptions" ? "Every event was verified" : "No events match"}
            description={focus === "exceptions" ? "Events that can't be traced to a trusted sender show up here." : "Try another filter or a longer time range."}
          />
        {:else}
          <DataTable>
            <thead><tr><th class="w-6"><span class="sr-only">Details</span></th><th>When</th><th>Event</th><th>From</th><th>Check</th><th class="text-right">Size</th></tr></thead>
            <tbody>
              {#each rows as row (`${row.streamSequence}:${row.eventId}`)}
                {@const check = eventCheckLabel(row.verificationStatus, row.resolution)}
                {@const open = openEventKey === eventKey(row)}
                <tr class="event-row" class:row-selected={open} data-event-type={`${row.ownerContractId ?? ""} / ${row.ownerEventName ?? ""}`} data-publisher={row.publisherDeploymentId} onclick={() => { void toggleEvent(row); }}>
                  <td><Icon name={open ? "chevronDown" : "chevronRight"} size={14} class="text-base-content/50" /></td>
                  <td class="whitespace-nowrap text-sm">{formatDate(row.eventTime)}</td>
                  <td class="min-w-0">
                    <button type="button" class="link link-hover block max-w-sm truncate text-left font-medium" aria-expanded={open} onclick={(event) => { event.stopPropagation(); void toggleEvent(row); }}>{eventLabel(row)}</button>
                    <span class="block text-xs text-base-content/60">{apiLabel(row.ownerContractId)}</span>
                  </td>
                  <td class="text-sm">
                    {#if row.publisherDeploymentId}<button type="button" class="link link-hover text-left" title="Show only events from this sender" onclick={(event) => { event.stopPropagation(); selectPublisher(row.publisherDeploymentId); }}>{publisherLabel(row)}</button>{:else}{publisherLabel(row)}{/if}
                  </td>
                  <td><StatusBadge label={check.label} status={check.ok ? "healthy" : row.resolution === "resolved" ? "degraded" : "unhealthy"} /></td>
                  <td class="text-right text-sm tabular-nums">{formatBytes(boundedNumber(row.payloadSizeBytes))}</td>
                </tr>
                {#if open}
                  <tr class="event-detail-row">
                    <td colspan="6">
                      {#if detailLoading}
                        <LoadingState label="Loading event" />
                      {:else if selectedEvent}
                        {@render eventDetail(selectedEvent)}
                      {/if}
                    </td>
                  </tr>
                {/if}
              {/each}
            </tbody>
          </DataTable>
          {#if cursorBackStack.length > 0 || nextCursor}
            <nav class="flex items-center justify-end gap-3 text-sm text-base-content/70" aria-label="Event pages">
              <button class="btn btn-outline btn-xs" onclick={goPrevious} disabled={cursorBackStack.length === 0}>Previous</button>
              <span>Page {cursorBackStack.length + 1}</span>
              <button class="btn btn-outline btn-xs" onclick={goNext} disabled={!nextCursor}>Next</button>
            </nav>
          {/if}
        {/if}
      </Panel>
    </div>

    <div class="events-grid">
      <Panel title="Delivery to services" class="consumer-health min-w-0">
        {#snippet actions()}
          {#if attentionConsumersOnly}
            <button type="button" class="btn btn-ghost btn-xs" onclick={() => { attentionConsumersOnly = false; }}>Show all</button>
          {/if}
        {/snippet}
        {#if consumerError}
          <Notice variant="warning" role="status">Delivery status can't be shown: {consumerError}</Notice>
        {:else if displayedConsumers.length === 0}
          <EmptyState
            title={attentionConsumersOnly ? "Every service is caught up" : "No services receive events"}
            description={attentionConsumersOnly ? "Nothing is waiting for delivery." : "Services that listen for events appear here."}
          />
        {:else}
          <ul class="service-list">
            {#each displayedConsumers as consumer (`${consumer.stream}:${consumer.consumerName}`)}
              {@const status = deliveryStatus(consumer.status)}
              {@const selected = selectedConsumer?.resourceId === consumer.resourceId}
              <li class:selected>
                <button type="button" class="service-row" aria-pressed={selected} onclick={() => selectConsumer(consumer)}>
                  <span class="min-w-0">
                    <span class="block truncate font-medium">{consumerName(consumer)}</span>
                    <span class="block truncate text-xs text-base-content/60">{consumerSummary(consumer)}</span>
                  </span>
                  <StatusBadge label={status.label} status={status.tone} />
                </button>
                {#if selected}
                  <div class="service-detail">
                    <p>{deliveryExplanation(consumer)}</p>
                    <dl>
                      <div><dt>Waiting</dt><dd>{consumer.pending.toLocaleString()}</dd></div>
                      <div><dt>In progress</dt><dd>{consumer.ackPending.toLocaleString()}</dd></div>
                      <div><dt>Sent again</dt><dd>{(consumer.redelivered ?? 0).toLocaleString()}</dd></div>
                      <div><dt>Oldest waiting</dt><dd>{consumer.oldestPendingAt ? ageLabel(consumer.oldestPendingAt) : "—"}</dd></div>
                    </dl>
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </Panel>

      <aside class="events-rail" aria-label="Event activity">
        <Panel title="Activity">
          {#snippet actions()}<span class="text-sm text-base-content/60">{windowOption.title}</span>{/snippet}
          {#if !metricsReady}
            <p class="py-10 text-center text-sm text-base-content/60">
              {visibleMetricsState === "pending" ? "Loading…" : `Activity can't be shown: ${metricsError ?? "not allowed"}`}
            </p>
          {:else if chartPoints.length === 0}
            <p class="py-10 text-center text-sm text-base-content/60">No events in this period.</p>
          {:else}
            <ColumnChart points={chartPoints} unit="events" />
          {/if}
        </Panel>
        <Panel title="Most published">
          {#if !metricsReady}
            <p class="text-sm text-base-content/60">{visibleMetricsState === "pending" ? "Loading…" : "Not available."}</p>
          {:else}
            <div class="event-types">
              {#each eventTypesByCount.slice(0, 6) as eventType (`${eventType.ownerContractId}:${eventType.ownerEventName}`)}
                {@const active = selectedEventType?.ownerContractId === eventType.ownerContractId && selectedEventType.ownerEventName === eventType.ownerEventName}
                <button class:active aria-pressed={active} title="Show only these events" onclick={() => selectEventType(eventType)}>
                  <span><strong>{sentenceName(eventType.ownerEventName)}</strong><small>{apiLabel(eventType.ownerContractId)}</small></span><b>{eventType.count.toLocaleString()}</b>
                  <i style={`--width: ${metrics?.summary.total ? boundedNumber(eventType.count) / boundedNumber(metrics.summary.total) * 100 : 0}%`}></i>
                </button>
              {:else}
                <p class="text-sm text-base-content/60">No events in this period.</p>
              {/each}
            </div>
          {/if}
        </Panel>
      </aside>
    </div>

    <div id="failed-deliveries">
      <Panel title="Failed deliveries">
        {#snippet actions()}
          {#if selectedConsumer}
            <button type="button" class="trellis-chip" onclick={() => selectConsumer(null)}>For {consumerName(selectedConsumer)} <span aria-hidden="true">×</span><span class="sr-only">Show all services</span></button>
          {/if}
        {/snippet}
        {#if deadLetterPanelError}
          <Notice variant="warning" role="status">Failed deliveries can't be shown: {deadLetterPanelError}</Notice>
        {:else if shownDeadLetters.length === 0}
          <EmptyState
            title="No failed deliveries"
            description={selectedConsumer ? "Everything sent to this service was delivered." : "When a service keeps failing to handle an event, it waits here for you to retry or dismiss it."}
          />
        {:else}
          <ul class="failure-list">
            {#each shownDeadLetters as deadLetter (deadLetter.deadLetterId)}
              {@const open = openDeadLetter === deadLetter.deadLetterId}
              <li data-dead-letter={deadLetter.deadLetterId}>
                <div class="failure-row">
                  <button type="button" class="failure-main" aria-expanded={open} onclick={() => void toggleDeadLetter(deadLetter)}>
                    <span class="font-medium">{deadLetterService(deadLetter)}</span>
                    <span class="block text-sm text-error">{errorText(deadLetter.lastError) ?? "The service didn't finish handling this event."}</span>
                    <span class="block text-xs text-base-content/60">Tried {deadLetter.deliveries} {deadLetter.deliveries === 1 ? "time" : "times"} · last {ageLabel(deadLetter.updatedAt)} ago{deadLetter.state === "dead" ? "" : " · retry in progress"}</span>
                  </button>
                  {#if deadLetter.state === "dead"}
                    <span class="flex shrink-0 gap-2">
                      <button class="btn btn-outline btn-sm" disabled={deadLetterBusy === deadLetter.deadLetterId} onclick={() => changeDeadLetter(deadLetter, "replay")}>Retry</button>
                      <button class="btn btn-ghost btn-sm text-error" disabled={deadLetterBusy === deadLetter.deadLetterId} onclick={() => changeDeadLetter(deadLetter, "dismiss")}>Dismiss</button>
                    </span>
                  {/if}
                </div>
                {#if deadLetterError[deadLetter.deadLetterId]}<p class="mt-2 text-sm text-error">{deadLetterError[deadLetter.deadLetterId]}</p>{/if}
                {#if open}
                  <div class="failure-detail">
                    {#if !deadLetterDetail}
                      {#if !deadLetterError[deadLetter.deadLetterId]}<p class="text-sm text-base-content/60">Loading…</p>{/if}
                    {:else}
                      {@const data = decodeData(deadLetterDetail.originalPayload)}
                      <dl class="fact-list">
                        <div><dt>Event</dt><dd>{sentenceName(subjectEventName(deadLetterDetail.originalSubject))}</dd></div>
                        <div><dt>Published</dt><dd>{formatDate(deadLetterDetail.originalPublishedAt)}</dd></div>
                      </dl>
                      <div class="mt-3">
                        {#if data?.kind === "value"}<DataPanel value={data.value} />
                        {:else}
                          <h3 class="text-sm font-semibold">Data</h3>
                          {#if data?.kind === "text"}<p class="text-sm whitespace-pre-wrap">{data.text}</p>
                          {:else}<p class="text-sm text-base-content/60">This event's data isn't readable text ({data?.kind === "bytes" ? formatBytes(data.size) : "empty"}).</p>{/if}
                        {/if}
                      </div>
                    {/if}
                  </div>
                {/if}
              </li>
            {/each}
          </ul>
        {/if}
      </Panel>
    </div>


  {/if}
  <ConfirmationModal bind:this={confirmationModal} />
</section>

<style>
  .events-page { display: grid; gap: 1.25rem; grid-template-columns: minmax(0, 1fr); }
  .events-grid { display: grid; gap: 1.25rem; grid-template-columns: minmax(0, 1fr) minmax(18rem, 24rem); align-items: start; }
  .events-rail { display: grid; gap: 1.25rem; min-width: 0; }
  .row-selected { background: color-mix(in oklab, var(--color-primary) 8%, var(--color-base-100)); }
  .service-list { display: grid; }
  .service-list > li { border-top: 1px solid var(--color-base-300); }
  .service-list > li:first-child { border-top: 0; }
  .service-row { align-items: center; background: transparent; border: 0; cursor: pointer; display: flex; gap: 1rem; justify-content: space-between; padding: 0.75rem 0.25rem; text-align: left; width: 100%; }
  .service-row:hover { background: color-mix(in oklab, var(--color-base-200) 60%, transparent); }
  .service-list > li.selected .service-row { background: color-mix(in oklab, var(--color-primary) 7%, transparent); }
  .service-detail { font-size: 0.875rem; padding: 0.25rem 0.25rem 0.9rem; }
  .service-detail p { color: color-mix(in oklab, var(--color-base-content) 75%, transparent); }
  .service-detail dl { display: grid; gap: 0.5rem; grid-template-columns: repeat(auto-fit, minmax(7rem, 1fr)); margin-top: 0.6rem; }
  .service-detail dt, .fact-list dt { color: color-mix(in oklab, var(--color-base-content) 60%, transparent); font-size: 0.8125rem; }
  .service-detail dd { font-size: 1rem; font-weight: 600; font-variant-numeric: tabular-nums; }
  .fact-list { display: grid; gap: 0.75rem 1.5rem; grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr)); }
  .fact-list dd { font-size: 0.9rem; overflow-wrap: anywhere; }
  .failure-list { display: grid; }
  .failure-list > li { border-top: 1px solid var(--color-base-300); padding: 0.85rem 0; }
  .failure-list > li:first-child { border-top: 0; padding-top: 0; }
  .failure-row { align-items: center; display: flex; gap: 1rem; justify-content: space-between; }
  .failure-main { background: transparent; border: 0; cursor: pointer; min-width: 0; padding: 0; text-align: left; }
  .failure-main:hover > span:first-child { text-decoration: underline; }
  .failure-detail { background: color-mix(in oklab, var(--color-base-200) 55%, transparent); border-radius: var(--radius-field); margin-top: 0.75rem; padding: 0.9rem 1rem; }
  .chip-row { align-items: center; display: flex; flex-wrap: wrap; gap: 0.4rem; margin-bottom: 0.75rem; }
  .event-row { cursor: pointer; }
  .event-row:hover { background: color-mix(in oklab, var(--color-base-200) 55%, transparent); }
  .event-detail-row > td { background: color-mix(in oklab, var(--color-base-200) 45%, transparent); padding: 1rem 1.25rem 1.25rem; }
  .event-types { display: grid; gap: 0.35rem; min-width: 0; }
  .event-types button { background: transparent; border: 0; border-radius: 0.5rem; cursor: pointer; display: grid; gap: 0.2rem 0.5rem; grid-template-columns: minmax(0, 1fr) auto; margin: 0 -0.35rem; padding: 0.4rem 0.35rem; text-align: left; width: calc(100% + 0.7rem); }
  .event-types button:hover, .event-types button.active { background: color-mix(in oklab, var(--color-base-200) 72%, transparent); }
  .event-types button span { min-width: 0; }
  .event-types button strong, .event-types button small { display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .event-types button strong { font-size: 0.875rem; font-weight: 500; }
  .event-types button small { color: color-mix(in oklab, var(--color-base-content) 60%, transparent); font-size: 0.75rem; }
  .event-types button b { font-size: 0.875rem; font-variant-numeric: tabular-nums; font-weight: 600; }
  .event-types button i { background: color-mix(in oklab, var(--color-primary) 55%, var(--color-base-300)); border-radius: 999px; grid-column: 1 / -1; height: 0.25rem; width: var(--width); }

  @media (max-width: 75rem) {
    .events-grid { grid-template-columns: minmax(0, 1fr); }
  }
</style>
