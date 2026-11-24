<script lang="ts">
  import { untrack } from "svelte";
  import type { apis } from "$lib/trellis-apis";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import NotAllowed from "$lib/components/NotAllowed.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import { boundedNumber, formatDate } from "$lib/format";

  type Report = apis.auth.RuntimeDiagnosticsOutput;
  type Usage = NonNullable<Report["jetstream"]["storage"]>;
  type Section = { status: string; findings: Report["server"]["findings"] };

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  let report = $state.raw<Report | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  const authorityState = $derived(authority.state);
  const canRead = $derived(
    canPerform(authority.authority, "runtimeDiagnostics"),
  );

  const STATUS = {
    ok: { label: "OK", icon: "check", tone: "text-success" },
    warning: { label: "Warning", icon: "alert", tone: "text-warning" },
    problem: { label: "Problem", icon: "close", tone: "text-error" },
  } as const;

  function status(value: string) {
    return value === "ok" || value === "warning" || value === "problem"
      ? STATUS[value]
      : { label: value, icon: "minus" as const, tone: "text-base-content/60" };
  }

  function bytes(value: bigint): string {
    const count = boundedNumber(value);
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let scaled = count;
    let unit = 0;
    while (scaled >= 1024 && unit < units.length - 1) {
      scaled /= 1024;
      unit += 1;
    }
    return unit === 0 ? `${count} B` : `${scaled.toFixed(1)} ${units[unit]}`;
  }

  function usage(value: Usage | undefined): string | undefined {
    if (!value) return undefined;
    const limit =
      value.limit === undefined ? "no limit" : `of ${bytes(value.limit)}`;
    return `${bytes(value.used)} used, ${bytes(value.reserved)} reserved ${limit}`;
  }

  function uptime(seconds: bigint): string {
    const total = boundedNumber(seconds);
    const days = Math.floor(total / 86_400);
    const hours = Math.floor((total % 86_400) / 3_600);
    const minutes = Math.floor((total % 3_600) / 60);
    if (days > 0) return `${days} d ${hours} h`;
    if (hours > 0) return `${hours} h ${minutes} min`;
    return `${minutes} min`;
  }

  const STATES: Record<string, string> = {
    healthy: "Healthy",
    degraded: "Degraded",
    unhealthy: "Unhealthy",
    unresponsive: "Not responding",
    offline: "Offline",
    starting: "Starting",
    waitingForSettings: "Waiting for settings",
    waitingForApproval: "Waiting for approval",
    unknown: "Unknown",
  };

  async function load(): Promise<void> {
    loading = true;
    error = null;
    try {
      report = await trellis
        .runtimeDiagnostics({}, { timeout: 20_000 })
        .orThrow();
    } catch (cause) {
      error = projectConsoleError(cause).message;
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    const ready = authorityState === "ready";
    const permitted = canRead;
    untrack(() => {
      if (!ready) {
        loading = authorityState === "checking";
        return;
      }
      if (!permitted) {
        loading = false;
        return;
      }
      void load();
    });
  });
</script>

{#snippet heading(title: string, section: Section)}
  {@const view = status(section.status)}
  <div class="flex items-center justify-between gap-3 p-4 pb-2">
    <h2 class="text-base font-semibold">{title}</h2>
    <span
      class={[
        "inline-flex items-center gap-1.5 text-sm font-medium",
        view.tone,
      ]}><Icon name={view.icon} size={16} />{view.label}</span
    >
  </div>
  {#if section.findings.length > 0}
    <ul class="space-y-2 px-4 pb-3">
      {#each section.findings as finding, index (index)}
        <li>
          <Notice variant={finding.status === "problem" ? "error" : "warning"}
            >{finding.message}</Notice
          >
        </li>
      {/each}
    </ul>
  {/if}
{/snippet}

{#snippet fields(rows: [string, string | undefined][])}
  <dl class="grid gap-x-6 gap-y-2 px-4 pb-4 text-sm sm:grid-cols-[auto_1fr]">
    {#each rows.filter(([, value]) => value !== undefined && value !== "") as [label, value] (label)}
      <dt class="text-base-content/60">{label}</dt>
      <dd class="min-w-0 break-words">{value}</dd>
    {/each}
  </dl>
{/snippet}

<section class="space-y-4">
  <PageToolbar
    title="Diagnostics"
    description="What needs attention in this Trellis, in one place."
  >
    {#snippet actions()}
      {#if canRead}
        <button
          type="button"
          class="btn btn-outline btn-sm"
          disabled={loading}
          onclick={() => void load()}
          ><Icon name="refresh" size={16} />Check again</button
        >
      {/if}
    {/snippet}
  </PageToolbar>

  {#if authorityState === "ready" && !canRead}
    <NotAllowed what="diagnostics" />
  {:else if loading && !report}
    <LoadingState label="Checking Trellis" />
  {:else if error}
    <Notice variant="error" role="alert">{error}</Notice>
  {:else if report}
    {@const overall = status(report.status)}
    <div
      class="trellis-section flex items-center gap-3 p-4"
      data-testid="diagnostics-status"
    >
      <span class={overall.tone}><Icon name={overall.icon} size={20} /></span>
      <p class="text-sm">
        {#if report.status === "ok"}
          Everything looks fine.
        {:else if report.status === "warning"}
          Some things need a look.
        {:else}
          Some things need fixing.
        {/if}
        <span class="text-base-content/60"
          >Checked {formatDate(report.asOf)}.</span
        >
      </p>
    </div>

    <div class="trellis-section" data-testid="diagnostics-server">
      {@render heading("Server", report.server)}
      {@render fields([
        ["Version", report.server.version],
        ["Installation", report.server.installationId],
        ["Mode", report.server.mode],
        ["Running for", uptime(report.server.uptimeSeconds)],
      ])}
    </div>

    <div class="trellis-section" data-testid="diagnostics-health">
      {@render heading("Health", report.health)}
      {@render fields(
        report.health.deployments.map((deployment) => [
          deployment.title,
          deployment.instances.length === 0
            ? "No instances"
            : deployment.instances
                .map((instance) => STATES[instance.state] ?? instance.state)
                .join(", "),
        ]),
      )}
    </div>

    <div class="trellis-section" data-testid="diagnostics-jetstream">
      {@render heading("Message storage", report.jetstream)}
      {@render fields([
        ["Storage", usage(report.jetstream.storage)],
        ["Server storage", usage(report.jetstream.serverStorage)],
        ["Memory", usage(report.jetstream.memory)],
        ["Server memory", usage(report.jetstream.serverMemory)],
      ])}
    </div>

    <div class="trellis-section" data-testid="diagnostics-disk">
      {@render heading("Disk", report.disk)}
      {@render fields([
        ["Folder", report.disk.path],
        [
          "Free",
          report.disk.free === undefined || report.disk.total === undefined
            ? undefined
            : `${bytes(report.disk.free)} of ${bytes(report.disk.total)}`,
        ],
      ])}
    </div>

    <div class="trellis-section" data-testid="diagnostics-certificates">
      {@render heading("Certificates", report.certificates)}
      {@render fields(
        report.certificates.certificates.length === 0
          ? [
              [
                "Served by",
                report.certificates.transport === "proxy"
                  ? "The proxy in front of Trellis"
                  : "None: plain HTTP on this computer only",
              ],
            ]
          : report.certificates.certificates.map((certificate) => [
              certificate.role === "served" ? "Served" : "Issuing CA",
              `${certificate.subject}, expires ${formatDate(certificate.notAfter)}`,
            ]),
      )}
    </div>

    <div class="trellis-section" data-testid="diagnostics-failures">
      {@render heading("Recent server failures", report.failures)}
      {#if report.failures.failures.length === 0}
        <p class="px-4 pb-4 text-sm text-base-content/60">
          None since the server started.
        </p>
      {:else}
        <ul class="space-y-2 px-4 pb-4 text-sm">
          {#each report.failures.failures as failure (failure.id)}
            <li class="min-w-0 break-words">
              <span class="text-base-content/60">{formatDate(failure.at)}</span>
              {failure.route ?? ""}
              {failure.code ? `· ${failure.code}` : ""}: {failure.error}
              <span
                class="trellis-identifier block text-xs text-base-content/60"
                >{failure.id}</span
              >
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <div class="trellis-section" data-testid="diagnostics-work">
      {@render heading("Background work", report.work)}
      {@render fields([
        [
          "Unfinished access changes",
          String(report.work.stuckEnforcement.length),
        ],
        ["Set aside", String(report.work.setAside.length)],
      ])}
    </div>
  {/if}
</section>
