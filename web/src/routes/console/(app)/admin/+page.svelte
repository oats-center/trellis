<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { resolve } from "$lib/console_paths";
  import ColumnChart from "$lib/components/ColumnChart.svelte";
  import HealthStatus from "$lib/components/HealthStatus.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import StatTile from "$lib/components/StatTile.svelte";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { IntervalTimer } from "$lib/console/live_refresh.ts";
  import { humanizeName } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Participant = apis.health.QueryOutput["items"][number];
  type EventMetrics = apis.events.MetricsOutput;
  type JobStats = apis.jobs.SummaryOutput["stats"];
  type Window = "1h" | "24h" | "7d";
  type Tone = "good" | "warning" | "bad" | "neutral";
  /** A panel's data, or why it is missing. */
  type Slot<T> =
    | { state: "loading" }
    | { state: "ready"; value: T }
    | { state: "not-allowed" }
    | { state: "error"; message: string };

  const trellis = getTrellis();
  const REFRESH_MS = 15_000;
  const LIST_LIMIT = 100;
  const WINDOWS: readonly { value: Window; label: string }[] = [
    { value: "1h", label: "1H" },
    { value: "24h", label: "24H" },
    { value: "7d", label: "7D" },
  ];

  let window = $state<Window>("24h");
  let health = $state.raw<
    Slot<{ total: bigint; healthy: bigint; participants: Participant[] }>
  >({ state: "loading" });
  let events = $state.raw<Slot<EventMetrics>>({ state: "loading" });
  let jobs = $state.raw<Slot<JobStats>>({ state: "loading" });
  let sessions = $state.raw<
    Slot<{ sessions: number; connections: number; more: boolean }>
  >({ state: "loading" });
  let pendingDevices = $state(0);
  let updatedAt = $state<Date | null>(null);

  const compact = new Intl.NumberFormat(undefined, { notation: "compact" });

  function failed<T>(error: unknown): Slot<T> {
    const projected = projectConsoleError(error);
    return projected.code === "not_allowed"
      ? { state: "not-allowed" }
      : { state: "error", message: projected.message };
  }

  // Each panel loads on its own; one failure never hides the others.
  async function settle<T>(work: () => Promise<T>): Promise<Slot<T>> {
    try {
      return { state: "ready", value: await work() };
    } catch (error) {
      return failed(error);
    }
  }

  async function load(): Promise<void> {
    const selected = window;
    const [nextHealth, nextEvents, nextJobs, nextSessions] = await Promise.all([
      settle(async () => {
        const [all, healthy, list] = await Promise.all([
          trellis.healthSummary({}).orThrow(),
          trellis.healthSummary({ statuses: ["healthy"] }).orThrow(),
          trellis.healthQuery({ page: { limit: LIST_LIMIT } }).orThrow(),
        ]);
        return {
          total: all.count,
          healthy: healthy.count,
          participants: list.items,
        };
      }),
      settle(() => trellis.eventsMetrics({ window: selected }).orThrow()),
      settle(
        async () =>
          (await trellis.jobsSummary({ groupBy: "type" }).orThrow()).stats,
      ),
      settle(async () => {
        const [sessionPage, connectionPage] = await Promise.all([
          trellis
            .authorizationSessionsList({ page: { limit: LIST_LIMIT } })
            .orThrow(),
          trellis.connectionsList({ page: { limit: LIST_LIMIT } }).orThrow(),
        ]);
        return {
          sessions: sessionPage.items.length,
          connections: connectionPage.items.length,
          more:
            sessionPage.page.nextCursor !== undefined ||
            connectionPage.page.nextCursor !== undefined,
        };
      }),
    ]);
    const reviews = await trellis
      .devicesReviewsList({ page: { limit: LIST_LIMIT } })
      .orThrow()
      .catch(() => null);
    if (selected !== window) return;
    pendingDevices =
      reviews?.items.filter((review) => review.state === "pending").length ?? 0;
    health = nextHealth;
    events = nextEvents;
    jobs = nextJobs;
    sessions = nextSessions;
    updatedAt = new Date();
  }

  const timer = new IntervalTimer({
    intervalMs: REFRESH_MS,
    canRun: () => !document.hidden,
    tick: () => load(),
  });

  onMount(() => {
    void load();
    timer.start();
    const onVisibility = () => {
      if (!document.hidden) void timer.refreshNow();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      timer.dispose();
    };
  });

  function count(value: bigint | number | undefined): number {
    return Number(value ?? 0);
  }

  function ago(iso: string): string {
    const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
    if (!Number.isFinite(seconds)) return "—";
    if (seconds < 60) return "just now";
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `${hours} h ago`;
    return `${Math.floor(hours / 24)} d ago`;
  }

  function bucketLabel(iso: string): { label: string; tick: string } {
    const date = new Date(iso);
    return {
      label: date.toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      }),
      tick:
        window === "7d"
          ? date.toLocaleDateString(undefined, { weekday: "short" })
          : date.toLocaleTimeString(undefined, {
              hour: "2-digit",
              minute: "2-digit",
            }),
    };
  }

  const unavailable = (slot: { state: string }) =>
    slot.state === "not-allowed" ? "Not allowed" : "Unavailable";

  const healthTile = $derived.by(
    (): { value: string; detail: string; tone: Tone; trend?: number[] } => {
      if (health.state !== "ready")
        return {
          value: "—",
          detail: health.state === "loading" ? "" : unavailable(health),
          tone: "neutral",
        };
      const total = count(health.value.total);
      const healthy = count(health.value.healthy);
      const issues = total - healthy;
      return {
        value: total ? `${healthy} of ${total}` : "0",
        detail: !total
          ? "Nothing reporting yet"
          : issues
            ? `${issues} need${issues === 1 ? "s" : ""} attention`
            : "All healthy",
        tone: !total ? "neutral" : issues ? "warning" : "good",
      };
    },
  );

  const eventsTile = $derived.by(
    (): { value: string; detail: string; tone: Tone; trend?: number[] } => {
      if (events.state !== "ready")
        return {
          value: "—",
          detail: events.state === "loading" ? "" : unavailable(events),
          tone: "neutral",
          trend: [] as number[],
        };
      const failedDeliveries = count(
        events.value.summary.deadLettersByState.dead,
      );
      return {
        value: compact.format(count(events.value.summary.total)),
        detail: failedDeliveries
          ? `${failedDeliveries} failed deliver${failedDeliveries === 1 ? "y" : "ies"}`
          : "No failed deliveries",
        tone: failedDeliveries ? "bad" : "good",
        trend: events.value.buckets.map((bucket) => count(bucket.total)),
      };
    },
  );

  const jobsTile = $derived.by(
    (): { value: string; detail: string; tone: Tone; trend?: number[] } => {
      if (jobs.state !== "ready")
        return {
          value: "—",
          detail: jobs.state === "loading" ? "" : unavailable(jobs),
          tone: "neutral",
        };
      const running = count(jobs.value.running);
      const waiting = count(jobs.value.queued);
      const failedJobs = count(jobs.value.dead) + count(jobs.value.failed);
      return {
        value: `${running} running`,
        detail: failedJobs
          ? `${failedJobs} failed · ${waiting} waiting`
          : `${waiting} waiting`,
        tone: failedJobs ? "bad" : "good",
      };
    },
  );

  const sessionsTile = $derived.by(() => {
    if (sessions.state !== "ready")
      return {
        value: "—",
        detail: sessions.state === "loading" ? "" : unavailable(sessions),
      };
    const more = sessions.value.more ? "+" : "";
    return {
      value: `${sessions.value.sessions}${more}`,
      detail: `${sessions.value.connections}${more} connected now`,
    };
  });

  type Attention = {
    key: string;
    title: string;
    detail: string;
    href: string;
    tone: "warning" | "bad";
  };

  const attention = $derived.by((): Attention[] => {
    const items: Attention[] = [];
    if (health.state === "ready") {
      for (const participant of health.value.participants) {
        if (participant.effectiveStatus === "healthy") continue;
        const total =
          count(participant.onlineInstances) +
          count(participant.offlineInstances);
        items.push({
          key: `health:${participant.participantKind}:${participant.contractId}`,
          title: `${humanizeName(participant.participantName)} is ${participant.effectiveStatus}`,
          detail: `${count(participant.onlineInstances)} of ${total} online · last seen ${ago(participant.lastSeenAt)}`,
          href: "/admin/health-events",
          tone: participant.effectiveStatus === "degraded" ? "warning" : "bad",
        });
      }
    }
    if (pendingDevices)
      items.push({
        key: "devices:pending",
        title: `${pendingDevices} device${pendingDevices === 1 ? "" : "s"} waiting for approval`,
        detail: "Check the code on each device, then approve or reject.",
        href: "/admin/devices?view=requests",
        tone: "warning",
      });
    if (events.state === "ready") {
      const dead = count(events.value.summary.deadLettersByState.dead);
      if (dead)
        items.push({
          key: "events:dead",
          title: `${dead} event${dead === 1 ? "" : "s"} failed to deliver`,
          detail: "A service couldn't process them. Retry or dismiss.",
          href: "/admin/events",
          tone: "bad",
        });
      const integrity = count(events.value.summary.integrityExceptions);
      if (integrity)
        items.push({
          key: "events:integrity",
          title: `${integrity} event${integrity === 1 ? "" : "s"} failed checks`,
          detail: "Malformed, unverified, or out of order.",
          href: "/admin/events?focus=exceptions",
          tone: "warning",
        });
    }
    if (jobs.state === "ready") {
      const dead = count(jobs.value.dead);
      if (dead)
        items.push({
          key: "jobs:dead",
          title: `${dead} job${dead === 1 ? "" : "s"} failed for good`,
          detail: "They won't retry on their own.",
          href: "/admin/jobs?focus=dead",
          tone: "bad",
        });
    }
    return items;
  });

  const chartPoints = $derived(
    events.state === "ready"
      ? events.value.buckets.map((bucket) => ({
          ...bucketLabel(bucket.start),
          value: count(bucket.total),
        }))
      : [],
  );

  const participants = $derived(
    health.state === "ready"
      ? [...health.value.participants]
          .sort(
            (left, right) =>
              Number(left.effectiveStatus === "healthy") -
                Number(right.effectiveStatus === "healthy") ||
              humanizeName(left.participantName).localeCompare(
                humanizeName(right.participantName),
              ),
          )
          .slice(0, 8)
      : [],
  );
</script>

<section class="space-y-6">
  <PageToolbar title="Overview" description="How Trellis is doing right now.">
    {#snippet actions()}
      {#if updatedAt}
        <span class="text-xs text-base-content/50"
          >Updated {updatedAt.toLocaleTimeString()}</span
        >
      {/if}
      <button
        type="button"
        class="btn btn-ghost btn-sm btn-square"
        aria-label="Refresh"
        onclick={() => void load()}><Icon name="refresh" size={16} /></button
      >
      <div class="trellis-segment" role="group" aria-label="Time range">
        {#each WINDOWS as option (option.value)}
          <button
            type="button"
            class={{ active: window === option.value }}
            aria-pressed={window === option.value}
            onclick={() => {
              window = option.value;
              events = { state: "loading" };
              void load();
            }}>{option.label}</button
          >
        {/each}
      </div>
    {/snippet}
  </PageToolbar>

  <div class="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
    <StatTile
      label="Health"
      icon="activity"
      href="/admin/health-events"
      {...healthTile}
    />
    <StatTile
      label={`Events · last ${window}`}
      icon="fileText"
      href="/admin/events"
      {...eventsTile}
    />
    <StatTile label="Jobs" icon="cog" href="/admin/jobs" {...jobsTile} />
    <StatTile
      label="Sessions"
      icon="users"
      href="/admin/sessions"
      {...sessionsTile}
    />
  </div>

  <div class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.25fr)]">
    <section class="trellis-section p-5">
      <h2 class="text-base font-semibold">Needs attention</h2>
      {#if attention.length}
        <ul class="mt-3 divide-y divide-base-300">
          {#each attention as item (item.key)}
            <li>
              <a
                href={resolve(item.href)}
                class="-mx-2 flex items-start gap-3 rounded-field px-2 py-3 hover:bg-base-200/60"
              >
                <span
                  class={[
                    "mt-0.5",
                    item.tone === "bad" ? "text-error" : "text-warning",
                  ]}><Icon name="alert" size={18} /></span
                >
                <span class="min-w-0 flex-1">
                  <span class="block text-sm font-medium">{item.title}</span>
                  <span class="block text-[0.8125rem] text-base-content/65"
                    >{item.detail}</span
                  >
                </span>
                <Icon
                  name="arrowRight"
                  size={16}
                  class="mt-1 text-base-content/40"
                />
              </a>
            </li>
          {/each}
        </ul>
      {:else if health.state === "loading"}
        <p class="mt-3 text-sm text-base-content/60">Checking…</p>
      {:else}
        <div class="flex items-center gap-3 py-8">
          <span
            class="grid h-10 w-10 place-items-center rounded-full bg-success/15 text-success"
            ><Icon name="check" size={20} /></span
          >
          <div>
            <p class="text-sm font-medium">Nothing needs attention</p>
            <p class="text-[0.8125rem] text-base-content/65">
              Everything you can see is healthy.
            </p>
          </div>
        </div>
      {/if}
    </section>

    <section class="trellis-section p-5">
      <div class="flex items-baseline justify-between">
        <h2 class="text-base font-semibold">Event activity</h2>
        <a
          href={resolve("/admin/events")}
          class="inline-flex items-center gap-1 text-sm text-base-content/70 hover:text-base-content"
          >View events <Icon name="arrowRight" size={14} /></a
        >
      </div>
      <p class="text-[0.8125rem] text-base-content/60">
        Events published, last {window}
      </p>
      <div class="mt-4">
        {#if events.state === "ready" && chartPoints.length}
          <ColumnChart points={chartPoints} unit="events" />
        {:else if events.state === "ready"}
          <p class="py-16 text-center text-sm text-base-content/60">
            No events in this period.
          </p>
        {:else if events.state === "loading"}
          <div
            class="h-[180px] animate-pulse rounded-field bg-base-200/70"
          ></div>
        {:else}
          <p class="py-16 text-center text-sm text-base-content/60">
            {events.state === "not-allowed"
              ? "You haven't allowed the Console to see events."
              : events.message}
          </p>
        {/if}
      </div>
    </section>
  </div>

  <section class="trellis-section">
    <div class="flex items-baseline justify-between px-5 pt-5">
      <h2 class="text-base font-semibold">Services and devices</h2>
      <a
        href={resolve("/admin/health-events")}
        class="inline-flex items-center gap-1 text-sm text-base-content/70 hover:text-base-content"
        >View all <Icon name="arrowRight" size={14} /></a
      >
    </div>
    {#if health.state === "ready" && participants.length}
      <div class="mt-3 overflow-x-auto">
        <table class="table trellis-table">
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
            {#each participants as participant (`${participant.participantKind}:${participant.contractId}`)}
              <tr>
                <td>
                  <div class="flex items-center gap-3">
                    <span
                      class="grid h-8 w-8 place-items-center rounded-full bg-base-200 text-base-content/70"
                      ><Icon
                        name={participant.participantKind === "device"
                          ? "laptop"
                          : "box"}
                        size={16}
                      /></span
                    >
                    <div>
                      <div class="font-medium">
                        {humanizeName(participant.participantName)}
                      </div>
                      <div class="text-xs text-base-content/60">
                        {participant.participantKind === "device"
                          ? "Device"
                          : "Service"}
                      </div>
                    </div>
                  </div>
                </td>
                <td><HealthStatus status={participant.effectiveStatus} /></td>
                <td class="tabular-nums"
                  >{count(participant.onlineInstances)} / {count(
                    participant.onlineInstances,
                  ) + count(participant.offlineInstances)}</td
                >
                <td class="text-base-content/70"
                  >{participant.versions.join(", ") || "—"}</td
                >
                <td class="text-base-content/70"
                  >{ago(participant.lastSeenAt)}</td
                >
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {:else}
      <p class="px-5 py-10 text-sm text-base-content/60">
        {health.state === "loading"
          ? "Loading…"
          : health.state === "not-allowed"
            ? "You haven't allowed the Console to see health."
            : health.state === "error"
              ? health.message
              : "Nothing is reporting health yet."}
      </p>
    {/if}
  </section>
</section>
