<script lang="ts">
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { onMount } from "svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import InlineMetricsStrip from "$lib/components/InlineMetricsStrip.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { errorMessage } from "$lib/format";
  import { loadJobsSummary } from "$lib/jobs_page.ts";
  import { IntervalTimer } from "$lib/console/live_refresh.ts";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";


  type ServiceInstance = apis.auth.ServiceInstancesListOutput["items"][number];
  type JobGroup = apis.jobs.SummaryOutput["groups"][number];
  type JobStats = apis.jobs.SummaryOutput["stats"];
  type DeviceReview = apis.auth.DeviceUserAuthoritiesReviewsListOutput["items"][number];
  type OverviewInstance = {
    service: string;
    id: string;
    status: "Enabled" | "Disabled";
    version: string;
    seen: string;
    type: "service" | "device" | "portal";
  };
  type OverviewJob = {
    key: string;
    job: string;
    state: string;
    count: number;
    oldest: string;
  };

  const trellis = getTrellis();

  let loading = $state(true);
  let error = $state<string | null>(null);
  let instances = $state<ServiceInstance[]>([]);
  let sessionCount = $state(0);
  let connectionCount = $state(0);
  let jobsUnavailableMessage = $state<string | null>(null);
  let sessionsPanelError = $state<string | null>(null);
  let connectionsPanelError = $state<string | null>(null);
  let instancesPanelError = $state<string | null>(null);
  let reviewsPanelError = $state<string | null>(null);
  let sessionsTruncated = $state(false);
  let connectionsTruncated = $state(false);
  let instancesTruncated = $state(false);
  let lastLoaded = $state<Date | null>(null);
  const OVERVIEW_PAGE_LIMIT = 100;
  const OVERVIEW_REFRESH_MS = 15_000;
  let jobGroups = $state.raw<JobGroup[]>([]);
  let jobStats = $state.raw<JobStats>({ byState: {}, total: 0n });
  let pendingDeviceReviews = $state.raw<DeviceReview[]>([]);

  const activeInstances = $derived(instances.filter((instance) => instance.state === "active").length);
  const disabledInstances = $derived(instances.filter((instance) => instance.state === "disabled").length);
  const displayInstances = $derived(instances.map(toOverviewInstance));
  const displayJobs = $derived(toOverviewJobs(jobGroups));
  const serviceInstanceTotal = $derived(instances.length);
  const disabledTotal = $derived(disabledInstances);
  const activeJobCount = $derived(jobStats.byState.active ?? 0);
  const totalJobCount = $derived(jobStats.total.toLocaleString());
  const pendingWorkTotal = $derived(pendingDeviceReviews.length);

  const topology = $derived([
    { icon: "box", label: "Service instances", value: serviceInstanceTotal, detail: `${activeInstances} enabled / ${disabledTotal} disabled`, tone: "text-neutral bg-base-300/60" },
    { icon: "users", label: "Sessions", value: sessionCount, detail: "live auth sessions", tone: "text-info bg-info/10" },
    { icon: "activity", label: "Connections", value: connectionCount, detail: "current transports", tone: "text-secondary bg-secondary/10" },
    { icon: "grid", label: "Jobs", value: totalJobCount, detail: `${activeJobCount} active`, tone: "text-warning bg-warning/10" },
  ]);

  const metrics = $derived([
    { label: "Service Instances", value: serviceInstanceTotal, detail: `/ ${disabledTotal} disabled` },
    { label: "Sessions", value: sessionCount },
    { label: "Connections", value: connectionCount },
    { label: "Jobs", value: totalJobCount, badge: `${activeJobCount} active`, badgeClass: "badge-neutral" },
  ]);

  function toOverviewInstance(instance: ServiceInstance): OverviewInstance {
    return {
      service: instance.deploymentId,
      id: instance.instanceId,
      status: instance.state === "disabled" ? "Disabled" : "Enabled",
      version: "—",
      seen: "known",
      type: "service",
    };
  }

  function toOverviewJobs(groups: JobGroup[]): OverviewJob[] {
    return groups.slice(0, 5).map((group) => ({
      key: group.key,
      job: group.label,
      state: group.state ? formatJobState(group.state) : "Grouped",
      count: Number(group.count > BigInt(Number.MAX_SAFE_INTEGER) ? BigInt(Number.MAX_SAFE_INTEGER) : group.count),
      oldest: group.oldestCreatedAt ?? "—",
    }));
  }

  function formatJobState(state: NonNullable<JobGroup["state"]>): OverviewJob["state"] {
    switch (state) {
      case "active":
        return "Active";
      case "retry":
        return "Retry";
      case "pending":
        return "Pending";
      case "completed":
        return "Completed";
      case "failed":
        return "Failed";
      case "cancelled":
        return "Cancelled";
      case "expired":
        return "Expired";
      case "dead":
        return "Dead";
      default:
        return state;
    }
  }

  function statusVariant(status: string): "healthy" | "degraded" | "unhealthy" | "offline" {
    if (status === "Healthy") return "healthy";
    if (status === "Degraded" || status === "Retry" || status === "Pending") return "degraded";
    if (status === "Failed" || status === "Dead") return "unhealthy";
    return "offline";
  }

  function toneForType(type: OverviewInstance["type"]): string {
    return {
      portal: "text-secondary bg-secondary/10",
      device: "text-info bg-info/10",
      service: "text-neutral bg-base-300/60",
    }[type];
  }

  async function load(showLoading = true) {
    if (showLoading) loading = true;
    error = null;
    jobsUnavailableMessage = null;
    try {
      // Each registry snapshot is independent: one failure must not hide the
      // other summaries. Counts are "N loaded" from this page, not instance-wide
      // totals; crawling every registry to invent a total is out of scope.
      error = null;
      const [sessionsRes, connectionsRes, instancesRes, deviceReviewsRes] = await Promise.allSettled([
        trellis.sessionsList({ page: { limit: OVERVIEW_PAGE_LIMIT } }).take(),
        trellis.connectionsList({ page: { limit: OVERVIEW_PAGE_LIMIT } }).take(),
        trellis.serviceInstancesList({ page: { limit: OVERVIEW_PAGE_LIMIT } }).take(),
        trellis.deviceUserAuthoritiesReviewsList({
          state: "pending",
          page: { limit: OVERVIEW_PAGE_LIMIT },
        }).take(),
      ]);
      if (sessionsRes.status === "fulfilled" && !isErr(sessionsRes.value)) {
        sessionCount = sessionsRes.value.items?.length ?? 0;
        sessionsTruncated = sessionsRes.value.page.nextCursor !== undefined;
        sessionsPanelError = null;
      } else {
        sessionsPanelError = errorMessage(
          sessionsRes.status === "rejected" ? sessionsRes.reason : sessionsRes.value,
        );
      }
      if (connectionsRes.status === "fulfilled" && !isErr(connectionsRes.value)) {
        connectionCount = connectionsRes.value.items?.length ?? 0;
        connectionsTruncated = connectionsRes.value.page.nextCursor !== undefined;
        connectionsPanelError = null;
      } else {
        connectionsPanelError = errorMessage(
          connectionsRes.status === "rejected" ? connectionsRes.reason : connectionsRes.value,
        );
      }
      if (instancesRes.status === "fulfilled" && !isErr(instancesRes.value)) {
        instances = instancesRes.value.items ?? [];
        instancesTruncated = instancesRes.value.page.nextCursor !== undefined;
        instancesPanelError = null;
      } else {
        instancesPanelError = errorMessage(
          instancesRes.status === "rejected" ? instancesRes.reason : instancesRes.value,
        );
      }
      if (deviceReviewsRes.status === "fulfilled" && !isErr(deviceReviewsRes.value)) {
        pendingDeviceReviews = deviceReviewsRes.value.items ?? [];
        reviewsPanelError = null;
      } else {
        reviewsPanelError = errorMessage(
          deviceReviewsRes.status === "rejected" ? deviceReviewsRes.reason : deviceReviewsRes.value,
        );
      }

      // Jobs summary uses the real Jobs.Summary RPC, not a synthetic aggregate.
      const jobsData = await loadJobsSummary(
        { summarizeJobs: (filter) => trellis.jobsSummary(filter) },
        { groupBy: "type" },
      ).catch((jobsError: unknown) => ({
        available: false as const,
        message: `Jobs summary is unavailable: ${errorMessage(jobsError)}`,
      }));
      jobGroups = jobsData.available ? jobsData.groups : [];
      jobStats = jobsData.available ? jobsData.stats : { byState: {}, total: 0n };
      jobsUnavailableMessage = jobsData.available
        ? null
        : jobsData.message ?? "Jobs summary is unavailable.";
      lastLoaded = new Date();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  // Periodically refresh while this route is visible and connected, one read
  // per panel at a time; suspend while the document is hidden.
  const overviewTimer = new IntervalTimer({
    intervalMs: OVERVIEW_REFRESH_MS,
    canRun: () => !document.hidden,
    tick: () => load(false),
  });

  onMount(() => {
    void load();
    overviewTimer.start();
    const onVisibility = () => {
      // The timer stays alive and simply skips ticks while hidden; refresh
      // immediately on visibility return instead of waiting a full interval.
      if (!document.hidden) void overviewTimer.refreshNow();
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      overviewTimer.dispose();
    };
  });
</script>

{#if loading}
  <LoadingState label="Loading overview" />
{:else}
  <section>
    <PageToolbar title="Overview" description="Real-time summary of your Trellis runtime">
      {#snippet actions()}
      <div class="flex items-center gap-2">
        <span class="text-xs text-base-content/50">
          {lastLoaded ? `Snapshot ${lastLoaded.toLocaleTimeString()}` : "Not loaded yet"}
        </span>
        <button class="btn btn-outline btn-sm" aria-label="Refresh" onclick={() => load()}>
          <Icon name="refresh" size={16} />
        </button>
      </div>
      {/snippet}
    </PageToolbar>

    {#if error}
      <Notice variant="error" class="mb-4">{error}</Notice>
    {/if}

    <Panel title="Runtime Topology" class="overflow-hidden">
      {#snippet actions()}
        <a href={resolve("/admin/health-events")} class="btn btn-ghost btn-sm gap-1">View topology <Icon name="arrowRight" size={16} /></a>
      {/snippet}
      <div class="divide-y divide-base-300 rounded-box border border-base-300">
          {#each topology as item (item.label)}
            <div class="grid grid-cols-[2rem_minmax(0,1fr)_auto] items-center gap-3 px-4 py-3 text-sm transition hover:bg-base-200/50">
              <div class={["grid h-8 w-8 shrink-0 place-items-center rounded-full", item.tone]}>
                <Icon name={item.icon} size={16} />
              </div>
              <div class="min-w-0"><div class="font-medium">{item.label}</div><div class="truncate text-xs text-base-content/55">{item.detail}</div></div>
              <div class="text-right font-semibold tabular-nums">{item.value}</div>
            </div>
          {/each}
      </div>
    </Panel>

    <InlineMetricsStrip metrics={metrics} class="mt-4" />

    <div class="mt-4 grid gap-4 xl:grid-cols-[minmax(0,1.2fr)_minmax(420px,0.8fr)]">
      <section class="trellis-section bg-base-100">
        <div class="trellis-section-body p-0">
          <div class="flex h-14 items-center justify-between border-b border-base-300 px-5">
            <h2 class="text-base font-semibold">Service Instances</h2>
            <a href={resolve("/admin/services")} class="btn btn-ghost btn-sm">View services <Icon name="arrowRight" size={16} /></a>
          </div>
          {#if displayInstances.length === 0}
            <EmptyState title="No service instances" description="Provisioned service instances will appear here after they are registered." class="m-5" />
          {:else}
          <DataTable class="min-w-[860px]" fixed>
              <colgroup>
                <col class="w-[28%]" />
                <col class="w-[32%]" />
                <col class="w-[14%]" />
                <col class="w-[10%]" />
                <col class="w-[12%]" />
                <col class="w-[4%]" />
              </colgroup>
              <thead><tr><th>Service</th><th>Instance ID</th><th>Status</th><th>Version</th><th>Last Seen</th><th></th></tr></thead>
              <tbody>
                {#each displayInstances as item (item.id)}
                  <tr>
                    <td>
                      <div class="flex min-w-0 items-center gap-3">
                        <span class={["grid h-8 w-8 shrink-0 place-items-center rounded-full", toneForType(item.type)]}><Icon name="box" size={16} /></span>
                        <span class="truncate font-medium" title={item.service}>{item.service}</span>
                      </div>
                    </td>
                    <td class="trellis-identifier truncate text-xs text-base-content/60" title={item.id}>{item.id}</td>
                    <td class="whitespace-nowrap"><StatusBadge label={item.status} status={statusVariant(item.status)} /></td>
                    <td class="whitespace-nowrap">{item.version}</td>
                    <td class="whitespace-nowrap">{item.seen}</td>
                    <td class="whitespace-nowrap"><button class="btn btn-ghost btn-xs btn-square" aria-label="More actions"><Icon name="more" size={16} /></button></td>
                  </tr>
                {/each}
              </tbody>
          </DataTable>
          {/if}
          <div class="flex h-14 items-center justify-between border-t border-base-300 px-5 text-sm text-base-content/60">
            <span>Showing {displayInstances.length === 0 ? "0" : `1–${displayInstances.length}`} of {serviceInstanceTotal}</span>
            <a href={resolve("/admin/services")} class="btn btn-ghost btn-sm">View service runtime <Icon name="arrowRight" size={16} /></a>
          </div>
        </div>
      </section>

      <div class="space-y-4">
        <section class="trellis-section bg-base-100">
          <div class="trellis-section-body gap-4 p-5">
            <div class="flex items-center justify-between"><h2 class="text-base font-semibold">Live Health</h2><a href={resolve("/admin/health-events")} class="btn btn-ghost btn-xs">View all</a></div>
            <EmptyState title="Live health opens in Health Events" description="Heartbeat-derived healthy, degraded, unhealthy, and offline states require the live health event stream." class="py-4" />
          </div>
        </section>

        <section class="trellis-section overflow-hidden bg-base-100">
          <div class="flex h-14 items-center justify-between border-b border-base-300 px-5"><h2 class="text-base font-semibold">Jobs Snapshot</h2><a href={resolve("/admin/jobs")} class="btn btn-ghost btn-xs">View all</a></div>
          {#if jobsUnavailableMessage}
            <div class="m-5 space-y-2">
              <Notice variant="info">{jobsUnavailableMessage}</Notice>
              <p class="text-xs text-base-content/60">Overview metrics remain available without the Jobs runtime.</p>
            </div>
          {:else if displayJobs.length === 0}
            <EmptyState title="No jobs" description="Job queues will appear here when the Jobs API reports active or retained work." class="m-5" />
          {:else}
          <DataTable size="xs">
              <thead><tr><th>Job</th><th>State</th><th>Count</th><th>Oldest</th></tr></thead>
              <tbody>
                {#each displayJobs as job (job.key)}
                  <tr><td class="trellis-identifier">{job.job}</td><td><StatusBadge label={job.state} status={statusVariant(job.state)} /></td><td>{job.count}</td><td>{job.oldest}</td></tr>
                {/each}
              </tbody>
          </DataTable>
          {/if}
        </section>

        <section class="trellis-section overflow-hidden bg-base-100">
          <div class="flex h-14 items-center justify-between border-b border-base-300 px-5"><h2 class="text-base font-semibold">Pending Work</h2><span class="badge badge-sm {pendingWorkTotal > 0 ? 'badge-warning' : 'badge-ghost'}">{pendingWorkTotal} pending</span></div>
          {#if pendingWorkTotal === 0}
            <div class="px-5 py-3 text-sm text-base-content/60">No device activation reviews waiting.</div>
          {:else}
            <div class="divide-y divide-base-300 text-sm">
              <a class="flex items-center justify-between px-5 py-2.5 hover:bg-base-200/50" href={resolve("/admin/devices")}>Device activation reviews <span class="badge badge-warning badge-sm">{pendingDeviceReviews.length}</span></a>
            </div>
          {/if}
        </section>
      </div>
    </div>

    <Panel title="Recent Activity" class="mt-4">
      {#snippet actions()}
        <a href={resolve("/admin/health-events")} class="btn btn-ghost btn-sm">View health events <Icon name="arrowRight" size={16} /></a>
      {/snippet}
        <EmptyState title="No activity feed connected" description="Use Health Events for the live heartbeat stream until activity events are available in the overview." class="py-4" />
    </Panel>
  </section>
{/if}
