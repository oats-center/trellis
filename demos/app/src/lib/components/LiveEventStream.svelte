<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { formatDateTimeWithAge } from "$lib/format";

  type AuditRecordedEvent = {
    activityId: string;
    kind: string;
    message: string;
    occurredAt: string;
  };
  type ReportsPublishedEvent = {
    reportId: string;
    inspectionId: string;
    publishedAt: string;
  };
  type SitesRefreshedEvent = {
    refreshId: string;
    site: { siteId?: string; siteName: string; latestStatus: string };
    refreshedAt: string;
  };
  type ActivityLiveFeedEvent =
    | { name: "Audit.Recorded"; event: AuditRecordedEvent }
    | { name: "Reports.Published"; event: ReportsPublishedEvent }
    | { name: "Sites.Refreshed"; event: SitesRefreshedEvent };
  type OperationName = "Sites.Refresh" | "Reports.Generate";
  type LiveEventKind = "event" | "operation" | "external-job";
  type LiveEvent = {
    id: string;
    kind: LiveEventKind;
    name: "Audit.Recorded" | "Reports.Published" | "Sites.Refreshed" | OperationName;
    action: string;
    subject: string;
    occurredAt: string;
    operationId?: string;
    refreshId?: string;
    inspectionId?: string;
    state?: string;
  };
  type OperationGroup = {
    kind: "operation-group";
    id: string;
    operationId: string;
    name: OperationName;
    latestAction: string;
    latestState: string;
    latestOccurredAt: string;
    children: LiveEvent[];
  };
  type StreamDisplayItem = LiveEvent | OperationGroup;
  type GroupedOperationLiveEvent = Omit<LiveEvent, "name" | "operationId"> & { name: OperationName; operationId: string };
  type LocalOperationUpdate = {
    kind: "operation" | "external-job";
    id: string;
    operationId: string;
    name: OperationName;
    action: string;
    subject: string;
    state: string;
    occurredAt: string;
    jobId?: string;
    refreshId?: string;
    inspectionId?: string;
  };

  const trellis = getTrellis();
  const feedRetryDelayMs = 1_500;

  let listening = $state(false);
  let starting = $state(false);
  let error = $state<string | null>(null);
  let liveEvents = $state<LiveEvent[]>([]);
  let controller: AbortController | null = null;
  let retryTimeout: ReturnType<typeof setTimeout> | null = null;
  let mounted = false;
  let localUpdateListener: EventListener | null = null;

  function isAuditRecordedEvent(value: unknown): value is AuditRecordedEvent {
    return typeof value === "object" && value !== null &&
      "activityId" in value && typeof value.activityId === "string" &&
      "kind" in value && typeof value.kind === "string" &&
      "message" in value && typeof value.message === "string" &&
      "occurredAt" in value && typeof value.occurredAt === "string";
  }

  function isReportsPublishedEvent(value: unknown): value is ReportsPublishedEvent {
    return typeof value === "object" && value !== null &&
      "reportId" in value && typeof value.reportId === "string" &&
      "inspectionId" in value && typeof value.inspectionId === "string" &&
      "publishedAt" in value && typeof value.publishedAt === "string";
  }

  function isSitesRefreshedEvent(value: unknown): value is SitesRefreshedEvent {
    return typeof value === "object" && value !== null &&
      "refreshId" in value && typeof value.refreshId === "string" &&
      "refreshedAt" in value && typeof value.refreshedAt === "string" &&
      "site" in value && typeof value.site === "object" && value.site !== null &&
      "siteName" in value.site && typeof value.site.siteName === "string" &&
      "latestStatus" in value.site && typeof value.site.latestStatus === "string";
  }

  function isActivityLiveFeedEvent(value: unknown): value is ActivityLiveFeedEvent {
    if (typeof value !== "object" || value === null || !("name" in value) || !("event" in value)) return false;
    if (value.name === "Audit.Recorded") return isAuditRecordedEvent(value.event);
    if (value.name === "Reports.Published") return isReportsPublishedEvent(value.event);
    if (value.name === "Sites.Refreshed") return isSitesRefreshedEvent(value.event);
    return false;
  }

  function isLocalOperationUpdate(value: unknown): value is LocalOperationUpdate {
    return typeof value === "object" && value !== null &&
      "kind" in value && (value.kind === "operation" || value.kind === "external-job") &&
      "id" in value && typeof value.id === "string" &&
      "operationId" in value && typeof value.operationId === "string" &&
      "name" in value && (value.name === "Sites.Refresh" || value.name === "Reports.Generate") &&
      "action" in value && typeof value.action === "string" &&
      "subject" in value && typeof value.subject === "string" &&
      "state" in value && typeof value.state === "string" &&
      "occurredAt" in value && typeof value.occurredAt === "string" &&
      (!("jobId" in value) || typeof value.jobId === "string") &&
      (!("refreshId" in value) || typeof value.refreshId === "string") &&
      (!("inspectionId" in value) || typeof value.inspectionId === "string");
  }

  function isOperationName(name: string): name is OperationName {
    return name === "Sites.Refresh" || name === "Reports.Generate";
  }

  function isGroupedOperationUpdate(event: LiveEvent): event is GroupedOperationLiveEvent {
    return isOperationName(event.name) && Boolean(event.operationId);
  }

  function addEvent(event: LiveEvent): void {
    if (!mounted) return;
    liveEvents = [event, ...liveEvents].slice(0, 24);
  }

  let displayItems = $derived.by((): StreamDisplayItem[] => {
    type DisplayBlock = { occurredAt: string; items: StreamDisplayItem[] };

    const groups: Record<string, OperationGroup> = {};
    const standaloneBlocks: DisplayBlock[] = [];

    for (const event of liveEvents) {
      if (isGroupedOperationUpdate(event)) {
        let group = groups[event.operationId];
        if (!group) {
          group = {
            kind: "operation-group",
            id: `operation-${event.operationId}`,
            operationId: event.operationId,
            name: event.name,
            latestAction: event.action,
            latestState: event.state ?? "event",
            latestOccurredAt: event.occurredAt,
            children: [],
          };
          groups[event.operationId] = group;
        }
        group.children.push(event);
      } else {
        standaloneBlocks.push({ occurredAt: event.occurredAt, items: [event] });
      }
    }

    const operationBlocks = Object.values(groups).map((group): DisplayBlock => {
      return { occurredAt: group.latestOccurredAt, items: [group] };
    });

    return [...operationBlocks, ...standaloneBlocks]
      .sort((left, right) => Date.parse(right.occurredAt) - Date.parse(left.occurredAt))
      .flatMap((block) => block.items);
  });

  function handleLocalOperationUpdate(event: Event): void {
    const detail = event instanceof CustomEvent ? event.detail : null;
    if (!isLocalOperationUpdate(detail)) return;
    addEvent({
      id: detail.id,
      kind: detail.kind,
      name: detail.name,
      action: detail.action,
      subject: detail.subject,
      occurredAt: detail.occurredAt,
      operationId: detail.operationId,
      refreshId: detail.refreshId ?? detail.jobId,
      inspectionId: detail.inspectionId,
      state: detail.state,
    });
  }

  function operationLabel(name: OperationName): string {
    return name.replace(".", " ");
  }

  function formatEventKind(kind: string): string {
    return kind
      .split(/[-_.\s]+/)
      .filter(Boolean)
      .map((part) => `${part.charAt(0).toUpperCase()}${part.slice(1)}`)
      .join(" ") || "Activity";
  }

  function subjectFromActivity(message: string): string {
    const keyMatch = message.match(/(?:from|upload)\s+(evidence\/\S+)/i);
    if (keyMatch) return keyMatch[1]?.split("/").pop() ?? "Evidence upload";
    return message;
  }

  function kindLabel(kind: LiveEventKind): string {
    if (kind === "external-job") return "EXTERNAL JOB";
    if (kind === "operation") return "UPDATE";
    return "EVENT";
  }

  function updateLabel(event: LiveEvent): string {
    if (event.kind === "operation" && event.state === "started") return "STARTED";
    if (event.kind === "operation" && event.state === "completed") return "COMPLETED";
    if (event.kind === "operation" && event.state === "failed") return "FAILED";
    return kindLabel(event.kind);
  }

  function handleAuditRecorded(event: AuditRecordedEvent): void {
    addEvent({
      id: `${event.activityId}-${event.occurredAt}`,
      kind: "event",
      name: "Audit.Recorded",
      action: formatEventKind(event.kind),
      subject: subjectFromActivity(event.message),
      occurredAt: event.occurredAt,
    });
  }

  function handleReportsPublished(event: ReportsPublishedEvent): void {
    addEvent({
      id: `${event.reportId}-${event.publishedAt}`,
      kind: "event",
      name: "Reports.Published",
      action: "Closeout Package Published",
      subject: event.inspectionId,
      occurredAt: event.publishedAt,
      inspectionId: event.inspectionId,
    });
  }

  function handleSitesRefreshed(event: SitesRefreshedEvent): void {
    addEvent({
      id: `${event.refreshId}-${event.refreshedAt}`,
      kind: "event",
      name: "Sites.Refreshed",
      action: "Site Status Refreshed",
      subject: `${event.site.siteName}: ${event.site.latestStatus}`,
      occurredAt: event.refreshedAt,
      refreshId: event.refreshId,
    });
  }

  function handleFeedEvent(value: unknown): void {
    if (!isActivityLiveFeedEvent(value)) {
      return;
    }
    if (value.name === "Audit.Recorded") handleAuditRecorded(value.event);
    if (value.name === "Reports.Published") handleReportsPublished(value.event);
    if (value.name === "Sites.Refreshed") handleSitesRefreshed(value.event);
  }

  function kindBadgeClass(kind: LiveEventKind): string {
    if (kind === "external-job") return "badge badge-accent badge-outline badge-sm max-w-full";
    if (kind === "operation") return "badge badge-secondary badge-outline badge-sm max-w-full";
    return "badge badge-primary badge-outline badge-sm max-w-full";
  }

  function clearRetryTimeout(): void {
    if (retryTimeout === null) return;
    clearTimeout(retryTimeout);
    retryTimeout = null;
  }

  function scheduleStartRetry(): void {
    if (retryTimeout !== null) return;
    retryTimeout = setTimeout(() => {
      retryTimeout = null;
      if (!mounted || controller !== null || listening || starting) return;
      error = null;
      void startListening();
    }, feedRetryDelayMs);
  }

  async function startListening(): Promise<void> {
    if (listening || starting) return;

    clearRetryTimeout();
    error = null;
    starting = true;
    controller = new AbortController();
    const localController = controller;
    let startupComplete = false;

    try {
      const stream = await trellis.auditFeed({}, { signal: localController.signal })
        .orThrow();

      if (!mounted || controller !== localController || localController.signal.aborted) return;
      startupComplete = true;
      listening = true;
      starting = false;

      for await (const event of stream) {
        if (!mounted || controller !== localController || localController.signal.aborted) break;
        handleFeedEvent(event);
      }
    } catch (cause) {
      localController.abort();
      if (controller !== localController) return;
      controller = null;
      listening = false;
      starting = false;
      if (!mounted) return;
      error = cause instanceof Error ? cause.message : String(cause);
      if (!startupComplete) scheduleStartRetry();
    } finally {
      if (controller === localController) {
        controller = null;
        listening = false;
        starting = false;
      } else if (controller === null) {
        starting = false;
      }
    }
  }

  function stopListening(): void {
    clearRetryTimeout();
    controller?.abort();
    controller = null;
    starting = false;
    listening = false;
  }

  onMount(() => {
    mounted = true;
    localUpdateListener = handleLocalOperationUpdate;
    window.addEventListener("trellisoperationupdate", localUpdateListener);
    void startListening();
  });

  onDestroy(() => {
    mounted = false;
    if (localUpdateListener) {
      window.removeEventListener("trellisoperationupdate", localUpdateListener);
      localUpdateListener = null;
    }
    stopListening();
  });
</script>

<section class="live-event-rail flex h-full min-w-0 flex-col gap-4 px-4 py-5" aria-label="Persistent live event stream">
  <header class="space-y-3">
    <div class="flex min-w-0 items-start justify-between gap-3">
      <div class="min-w-0">
        <p class="trellis-kicker">Live stream</p>
        <h2 class="mt-1 break-words text-lg font-black tracking-tight">System loop</h2>
      </div>
      <span class={listening ? "badge badge-success badge-outline max-w-full" : "badge badge-warning badge-outline max-w-full"}>
        <span class="truncate">{listening ? "live" : starting ? "starting" : "offline"}</span>
      </span>
    </div>
  </header>

  <p class="capability-note">
    <strong>Feed + operations:</strong> Audit.Feed + Sites.Refresh + Reports.Generate
  </p>

  {#if error}
    <div role="alert" class="alert alert-error py-2 text-sm"><span>{error}</span></div>
  {/if}

  <div class="min-h-0 flex-1 overflow-y-auto" aria-live="polite">
    {#if displayItems.length === 0}
      <div class="alert py-3 text-sm">
        <span>Authorized feed frames and operation updates will appear here as the workflow runs.</span>
      </div>
    {:else}
      <div class="divide-y divide-base-300/80 border-y border-base-300/80">
        {#each displayItems as item (item.id)}
          {#if item.kind === "operation-group"}
            <details class="min-w-0 border border-secondary/45 bg-base-200/30" open>
              <summary class="grid cursor-pointer list-none gap-1.5 px-3 py-2.5 text-sm marker:hidden">
                <div class="flex min-w-0 flex-wrap items-center justify-between gap-2">
                  <span class="flex min-w-0 items-center gap-2">
                    <svg class="collapse-chevron h-3.5 w-3.5 shrink-0 text-base-content/60" aria-hidden="true" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="m6 4 4 4-4 4" />
                    </svg>
                    <span class="badge badge-secondary badge-outline badge-sm max-w-full"><span class="truncate">OPERATION</span></span>
                  </span>
                  <span class="shrink-0 text-[0.68rem] uppercase tracking-[0.12em] text-base-content/48">{operationLabel(item.name)}</span>
                </div>
                <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-1">
                  <h3 class="min-w-0 truncate font-semibold">{item.name}</h3>
                  <span class="min-w-0 truncate text-xs text-base-content/70">{item.latestAction}</span>
                </div>
                <div class="flex min-w-0 flex-wrap items-center justify-between gap-2">
                  <span class="min-w-0 truncate font-mono text-[0.68rem] uppercase tracking-[0.08em] text-base-content/58">state {item.latestState}</span>
                  <span class="break-words text-[0.68rem] text-base-content/58">{formatDateTimeWithAge(item.latestOccurredAt)}</span>
                </div>
              </summary>
              <div class="divide-y divide-base-300/70 border-t border-base-300/80 pl-7">
                {#each item.children as child (child.id)}
                  <article class="min-w-0 py-2 pl-3 pr-3">
                    <div class="grid min-w-0 gap-1 text-xs">
                      <div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                        <span class={kindBadgeClass(child.kind)}><span class="truncate">{updateLabel(child)}</span></span>
                        <h4 class="min-w-0 truncate text-sm font-semibold">{child.action}</h4>
                        {#if child.state && child.kind !== "operation" && child.kind !== "external-job"}
                          <span class="min-w-0 truncate font-mono text-[0.65rem] uppercase tracking-[0.08em] text-base-content/54">state {child.state}</span>
                        {/if}
                      </div>
                      <div class="flex min-w-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 text-[0.68rem] leading-4 text-base-content/58">
                        <span class="min-w-0 truncate">{child.kind === "external-job" ? `Job ID: ${child.subject}` : child.subject}</span>
                        <span class="break-words">{formatDateTimeWithAge(child.occurredAt)}</span>
                      </div>
                    </div>
                  </article>
                {/each}
              </div>
            </details>
          {:else}
            <article class="min-w-0 border border-base-300/80 bg-base-200/30 py-2.5 pl-3 pr-3">
              <div class="grid min-w-0 gap-1.5 text-sm">
                <div class="flex min-w-0 flex-wrap items-center gap-2">
                  <span class={kindBadgeClass(item.kind)}><span class="truncate">{kindLabel(item.kind)}</span></span>
                  <h3 class="min-w-0 truncate font-semibold">{item.action}</h3>
                </div>
                <p class="min-w-0 truncate text-xs leading-5 text-base-content/64">{item.subject}</p>
                <div class="flex min-w-0 flex-wrap items-center justify-between gap-2 text-[0.68rem] text-base-content/58">
                  {#if item.state}
                    <span class="min-w-0 truncate font-mono uppercase tracking-[0.08em]">state {item.state}</span>
                  {/if}
                  <span class="break-words">{formatDateTimeWithAge(item.occurredAt)}</span>
                </div>
              </div>
            </article>
          {/if}
        {/each}
      </div>
    {/if}
  </div>
</section>

<style>
  details[open] .collapse-chevron {
    transform: rotate(90deg);
  }

  .collapse-chevron {
    transition: transform 120ms ease;
  }
</style>
