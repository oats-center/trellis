<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { afterNavigate } from "$app/navigation";
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import { formatDateTimeWithAge } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type ReportRecord = {
    reportId: string;
    inspectionId: string;
    siteId?: string;
    siteName: string;
    assetName: string;
    status: string;
    publishedAt: string;
    summary: string;
    reportComment: string;
    readiness: string;
    evidenceStatus: string;
  };

  const trellis = getTrellis();

  type InspectionRoute = "/inspection" | `/inspection?${string}`;

  let reports = $state<ReportRecord[]>([]);
  let selectedReportId = $state<string | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let mounted = false;
  let requestId = 0;
  const listPage = { page: { limit: 50 } };

  let selectedReport = $derived(
    reports.find((report) => report.reportId === selectedReportId) ?? reports[0] ?? null,
  );
  let selectedInspectionRoute = $derived.by((): InspectionRoute => {
    if (!selectedReport) return "/inspection";
    const params = new URLSearchParams({ inspectionId: selectedReport.inspectionId });
    if (selectedReport.siteId) params.set("siteId", selectedReport.siteId);
    return `/inspection?${params.toString()}` as InspectionRoute;
  });

  function selectReportFromUrl(loadedReports: ReportRecord[]): void {
    const reportId = page.url.searchParams.get("reportId");
    selectedReportId = loadedReports.some((report) => report.reportId === reportId)
      ? reportId
      : loadedReports[0]?.reportId ?? null;
  }

  async function loadReports(): Promise<void> {
    const runId = ++requestId;
    loading = true;
    error = null;

    try {
      const response = await trellis.reportsList(listPage).orThrow();
      if (!mounted || runId !== requestId) return;
      const loadedReports: ReportRecord[] = response.items;
      reports = loadedReports;
      selectReportFromUrl(loadedReports);
    } catch (cause) {
      if (!mounted || runId !== requestId) return;
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      if (!mounted || runId !== requestId) return;
      loading = false;
    }
  }

  onMount(() => {
    mounted = true;
    void loadReports();
  });

  afterNavigate(() => {
    if (!mounted || loading) return;
    selectReportFromUrl(reports);
  });

  onDestroy(() => {
    mounted = false;
    requestId += 1;
  });
</script>

<svelte:head>
  <title>Reports · Field Inspection Desk</title>
</svelte:head>

<section class="page-sheet rounded-box p-5 sm:p-7">
  <div class="flex flex-col gap-6">
    <header class="pb-1">
      <div class="flex min-w-0 flex-col gap-5 lg:flex-row lg:items-end lg:justify-between">
        <div class="min-w-0 space-y-3">
          <div class="trellis-kicker">Reports.List</div>
          <h1 class="break-words text-2xl font-black tracking-tight md:text-3xl">Completed closeout reports</h1>
          <p class="max-w-3xl break-words text-sm text-base-content/70">
            Review reports published by the inspection closeout workflow during this demo service run.
          </p>
        </div>
        <button class="btn btn-accent btn-sm" onclick={loadReports} disabled={loading}>
          {loading ? "Loading reports..." : "Refresh reports"}
        </button>
      </div>
      <p class="capability-note mt-4">
        <strong>RPC:</strong> Reports.List
      </p>
    </header>

    {#if error}
      <div role="alert" class="alert alert-error"><span>{error}</span></div>
    {/if}

    {#if loading && reports.length === 0}
      <div class="alert" role="status"><span>Loading completed reports from Reports.List.</span></div>
    {:else if reports.length === 0}
      <div class="next-action-rail px-1 py-4">
        <p class="source-label">No completed reports</p>
        <p class="mt-2 max-w-2xl text-sm text-base-content/68">
          Run closeout from an inspection to publish a report, then return here to view it.
        </p>
        <div class="mt-4">
          <a class="btn btn-accent btn-sm" href={resolve("/inspection")}>Open inspections</a>
        </div>
      </div>
    {:else}
      <div class="section-rule grid gap-7 pt-6 xl:grid-cols-[minmax(0,0.95fr)_minmax(22rem,1.05fr)]">
        <section class="min-w-0">
          <div class="flex min-w-0 items-center justify-between gap-3">
            <h2 class="min-w-0 break-words text-lg font-black tracking-tight">Report ledger</h2>
            <span class="source-label">Live Trellis response</span>
          </div>
          <div class="mt-5 overflow-x-auto">
            <table class="table table-zebra executive-table min-w-[36rem]">
              <thead>
                <tr><th>Report</th><th>Inspection</th><th>Status</th></tr>
              </thead>
              <tbody>
                {#each reports as report (report.reportId)}
                  <tr
                    class={[
                      "cursor-pointer hover:bg-base-200/80 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-accent",
                      selectedReport?.reportId === report.reportId && "assignment-row-selected",
                    ]}
                    role="button"
                    tabindex="0"
                    aria-label={`View report ${report.reportId}`}
                    aria-pressed={selectedReport?.reportId === report.reportId}
                    onclick={() => selectedReportId = report.reportId}
                    onkeydown={(event) => {
                      if (event.key !== "Enter" && event.key !== " ") return;
                      event.preventDefault();
                      selectedReportId = report.reportId;
                    }}
                  >
                    <th scope="row">
                      <div class="break-words font-medium">{report.siteName}</div>
                      <div class="break-words font-mono text-xs text-base-content/60">{report.reportId}</div>
                    </th>
                    <td>
                      <div class="break-words">{report.assetName}</div>
                      <div class="break-words font-mono text-xs text-base-content/60">{report.inspectionId}</div>
                    </td>
                    <td><span class="badge badge-success badge-outline">{report.status}</span></td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        </section>

        <section class="min-w-0 border-t border-base-300/80 pt-6 xl:border-l xl:border-t-0 xl:pl-6 xl:pt-0">
          {#if selectedReport}
            <div class="flex min-w-0 flex-col gap-5">
              <div class="min-w-0">
                <p class="source-label">Report view</p>
                <h2 class="mt-1 break-words text-lg font-black tracking-tight">{selectedReport.siteName}</h2>
                <p class="mt-1 break-words text-sm text-base-content/62">{formatDateTimeWithAge(selectedReport.publishedAt)}</p>
              </div>

              <dl class="divide-y divide-base-300/80 border-y border-base-300/80 text-sm">
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Report id</dt>
                  <dd class="break-words font-mono text-xs">{selectedReport.reportId}</dd>
                </div>
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Inspection</dt>
                  <dd class="break-words font-mono text-xs">{selectedReport.inspectionId}</dd>
                </div>
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Summary</dt>
                  <dd class="break-words">{selectedReport.summary}</dd>
                </div>
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Report comment</dt>
                  <dd class="break-words">{selectedReport.reportComment}</dd>
                </div>
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Readiness</dt>
                  <dd class="break-words">{selectedReport.readiness}</dd>
                </div>
                <div class="grid gap-1 py-3 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-4">
                  <dt class="source-label">Evidence</dt>
                  <dd class="break-words">{selectedReport.evidenceStatus}</dd>
                </div>
              </dl>

              <div class="next-action-rail px-1 py-4">
                <p class="source-label">Related workflow</p>
                <div class="mt-3 flex flex-wrap gap-3">
                  <a class="btn btn-outline btn-sm" href={resolve(selectedInspectionRoute)}>Open inspection</a>
                </div>
              </div>
            </div>
          {/if}
        </section>
      </div>
    {/if}
  </div>
</section>
