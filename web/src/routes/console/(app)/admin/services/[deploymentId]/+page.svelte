<script lang="ts">
  import { isErr } from "@oatscenter/result";
  import { formatDate, humanizeName } from "$lib/format";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { apis } from "$lib/trellis-apis";
  import { onMount, tick } from "svelte";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { resolve } from "$lib/console_paths";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { instanceIdentities } from "$lib/console/instance_identity.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import HealthStatus from "$lib/components/HealthStatus.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import ServiceUpdates from "$lib/components/ServiceUpdates.svelte";
  import ServiceUpdateHistory from "$lib/components/ServiceUpdateHistory.svelte";
  import ParticipantSettings from "$lib/components/ParticipantSettings.svelte";

  type Deployment = apis.auth.DeploymentsGetOutput["deployment"];
  type Instance = apis.auth.ServiceInstancesListOutput["items"][number];
  type Claim = apis.auth.ServiceClaimsListOutput["items"][number];
  type Update = apis.auth.ServiceUpdatesGetOutput["update"];
  type Health = apis.health.InspectOutput;
  type Running = Health["instances"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("service-deployment");
  const deploymentId = $derived(page.params.deploymentId ?? "");
  let deployment = $state.raw<Deployment | null>(null);
  // A service added without its IDL, until the service connects and is approved.
  let claim = $state.raw<Claim | null>(null);
  let instances = $state.raw<Instance[]>([]);
  // What each instance last reported, when health is visible; optional.
  let health = $state.raw<Health | null>(null);
  let updates = $state.raw<Update[]>([]);
  let capabilityTitles = $state.raw<Map<string, string>>(new Map());
  let loading = $state(true);
  let error = $state<string | null>(null);
  let instanceError = $state<string | null>(null);
  let busy = $state(false);
  let uncertain = $state(false);
  let adding = $state(false);
  let showRemoved = $state(false);
  let title = $state("");
  let identityPublicKey = $state("");
  let receipt = $state.raw<apis.auth.ServiceInstancesProvisionOutput | null>(
    null,
  );
  let settingsInstance = $state("");
  let settingsAvailable = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const identities = $derived(
    instanceIdentities(
      instances,
      new Map(
        deployment
          ? [[deployment.deploymentId, humanizeName(deployment.title)]]
          : [],
      ),
    ),
  );
  const instanceNames = $derived(
    new Map([...identities].map(([id, identity]) => [id, identity.name])),
  );
  const running = $derived(
    new Map<string, Running>(
      (health?.instances ?? [])
        .filter((item) => item.deploymentId === deployment?.deploymentId)
        .map((item) => [item.instanceId, item]),
    ),
  );
  const current = $derived(
    instances.filter((instance) => instance.state !== "removed"),
  );
  const removed = $derived(
    instances.filter((instance) => instance.state === "removed"),
  );
  const listed = $derived(showRemoved ? instances : current);
  const turnedOn = $derived(
    current.filter((instance) => instance.state === "active").length,
  );
  const connected = $derived(
    current.filter((instance) => {
      const report = running.get(instance.instanceId);
      return report !== undefined && report.effectiveStatus !== "offline";
    }).length,
  );
  // The deployment names its installed build. An administrator can apply a
  // build directly, which leaves no update behind, so the history only dates
  // the build when one of its applied updates installed exactly that build.
  const installedDigest = $derived(deployment?.implementationDigest);
  const installed = $derived(
    updates.find(
      (update) =>
        update.state === "applied" &&
        update.participantDigest === installedDigest,
    ),
  );
  // The installed build, by the version an instance running it reports.
  const installedText = $derived.by(() => {
    const version = [...running.values()].find(
      (item) => item.contractDigest === installedDigest,
    )?.latestSample.participant.version;
    const build = buildLabel(installedDigest, version);
    return installed?.decidedAt
      ? `${build} installed ${formatDate(installed.decidedAt)}`
      : build;
  });

  $effect(() => {
    const id = deploymentId;
    if (scope.setKey(id)) {
      deployment = null;
      claim = null;
      receipt = null;
      health = null;
      updates = [];
      adding = false;
      uncertain = false;
      settingsInstance = "";
    }
    void load(id);
  });
  async function load(id: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const input = apis.auth.API.actions["rpc:Deployments.Get"].input.decode({
        deploymentId: id,
      });
      const result = await trellis.deploymentsGet(input).orThrow();
      if (!scope.isCurrent(token)) return;
      if (result.deployment.kind !== "service")
        throw new Error("This exact deployment is not a service deployment.");
      deployment = result.deployment;
      const [listed, reported] = await Promise.all([
        traverseAll(async (page) => {
          const response = await trellis
            .serviceInstancesList({
              deploymentId: result.deployment.deploymentId,
              page,
            })
            .orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        loadHealth(result.deployment),
      ]);
      if (!scope.isCurrent(token)) return;
      instances = [...listed.items];
      health = reported;
      instanceError = listed.complete
        ? null
        : projectConsoleError(listed.error).message;
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      let found: Claim | undefined;
      try {
        found = await findClaim(id);
      } catch (lookup) {
        if (scope.isCurrent(token)) error = projectConsoleError(lookup).message;
        return;
      }
      if (!scope.isCurrent(token)) return;
      if (found?.state === "claimed" && found.deploymentId) {
        await goto(
          resolve("/admin/services/[deploymentId]", {
            deploymentId: found.deploymentId,
          }),
          { replaceState: true },
        );
        return;
      }
      if (found?.state === "waiting") claim = found;
      else error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }
  // Health is context for the instances; the page stands without it.
  async function loadHealth(target: Deployment): Promise<Health | null> {
    try {
      const result = await trellis
        .healthInspect({
          participantKind: "service",
          contractId: target.participantId,
          historyLimit: 1n,
        })
        .take();
      return isErr(result) ? null : result;
    } catch {
      return null;
    }
  }
  async function findClaim(id: string): Promise<Claim | undefined> {
    const listed = await traverseAll(async (page) => {
      const response = await trellis.serviceClaimsList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    const found = listed.items.find((item) => item.claimId === id);
    if (!found && !listed.complete) throw listed.error;
    return found;
  }
  async function removeClaim(): Promise<void> {
    if (
      !claim ||
      busy ||
      !canPerform(authority.authority, "serviceClaimsRemove")
    )
      return;
    const target = claim;
    const confirmed = await confirmationModal?.confirm({
      title: "Remove this service?",
      message:
        "Its identity stops working, and anything it asked for is turned down.",
      confirmLabel: "Remove",
      targetLabel: "Service",
      targetName: humanizeName(target.title),
    });
    if (!confirmed || scope.disposed || claim?.claimId !== target.claimId)
      return;
    busy = true;
    try {
      await trellis
        .serviceClaimsRemove({
          claimId: target.claimId,
          expectedRevision: target.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      await goto(resolve("/admin/services"));
    } catch (cause) {
      if (!scope.disposed) error = projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  async function provision(): Promise<void> {
    if (
      !deployment ||
      busy ||
      uncertain ||
      loading ||
      deployment.state !== "enabled" ||
      !canPerform(authority.authority, "serviceInstancesProvision")
    )
      return;
    const target = deployment;
    const route = scope.key;
    busy = true;
    error = null;
    try {
      const input = apis.auth.API.actions[
        "rpc:ServiceInstances.Provision"
      ].input.decode({
        deploymentId: target.deploymentId,
        title: title.trim(),
        idempotencyKey: ulid(),
        ...(identityPublicKey.trim()
          ? { identityPublicKey: identityPublicKey.trim() }
          : {}),
      });
      const result = await trellis.serviceInstancesProvision(input).orThrow();
      if (scope.disposed || scope.key !== route) return;
      receipt = result;
      title = "";
      identityPublicKey = "";
      adding = false;
      await load(target.deploymentId);
    } catch (cause) {
      if (scope.disposed || scope.key !== route) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Provisioning outcome unknown. Inspect service instances; a one-time secret cannot be recovered."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  type InstanceChange = "enable" | "disable" | "remove";
  type ServiceChange = "enable" | "disable" | "remove";
  const INSTANCE_OPERATIONS = {
    enable: "serviceInstancesEnable",
    disable: "serviceInstancesDisable",
    remove: "serviceInstancesRemove",
  } as const;
  const SERVICE_OPERATIONS = {
    enable: "deploymentsEnable",
    disable: "deploymentsDisable",
    remove: "deploymentsRemove",
  } as const;

  // Runs one change against the exact record it was asked for, then reloads.
  async function mutate(run: () => Promise<unknown>): Promise<void> {
    if (!deployment) return;
    const target = deployment;
    const route = scope.key;
    busy = true;
    error = null;
    try {
      await run();
      if (!scope.disposed && scope.key === route)
        await load(target.deploymentId);
    } catch (cause) {
      if (scope.disposed || scope.key !== route) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "We couldn't confirm the change. Refresh before trying again."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  async function changeInstance(
    instance: Instance,
    change: InstanceChange,
  ): Promise<void> {
    if (
      !deployment ||
      busy ||
      uncertain ||
      loading ||
      !canPerform(authority.authority, INSTANCE_OPERATIONS[change])
    )
      return;
    const name = identities.get(instance.instanceId)?.name ?? "";
    if (change !== "enable") {
      const confirmed = await confirmationModal?.confirm(
        change === "disable"
          ? {
              title: "Turn off this instance?",
              message:
                "It is disconnected and can't connect again until it is turned back on.",
              confirmLabel: "Turn off",
              targetLabel: "Instance",
              targetName: name,
            }
          : {
              title: "Remove this instance?",
              message:
                "Its identity stops working for good. The service and its other instances keep running.",
              confirmLabel: "Remove instance",
              targetLabel: "Instance",
              targetName: name,
              expectedValue: name,
            },
      );
      if (!confirmed || scope.disposed) return;
    }
    const input = {
      instanceId: instance.instanceId,
      expectedRevision: instance.revision,
      idempotencyKey: ulid(),
    };
    await mutate(() =>
      change === "enable"
        ? trellis.serviceInstancesEnable(input).orThrow()
        : change === "disable"
          ? trellis.serviceInstancesDisable(input).orThrow()
          : trellis.serviceInstancesRemove(input).orThrow(),
    );
  }

  async function changeService(change: ServiceChange): Promise<void> {
    if (
      !deployment ||
      busy ||
      uncertain ||
      loading ||
      !canPerform(authority.authority, SERVICE_OPERATIONS[change])
    )
      return;
    const target = deployment;
    const name = humanizeName(target.title);
    if (change !== "enable") {
      const confirmed = await confirmationModal?.confirm(
        change === "disable"
          ? {
              title: "Disable this service?",
              message:
                "Every instance is disconnected and can't connect until the service is enabled again.",
              confirmLabel: "Disable service",
              targetLabel: "Service",
              targetName: name,
              expectedValue: name,
            }
          : {
              title: "Remove this service?",
              message:
                "Its instances stop working for good and it leaves the services list. Its stored data is kept.",
              confirmLabel: "Remove service",
              targetLabel: "Service",
              targetName: name,
              expectedValue: name,
            },
      );
      if (
        !confirmed ||
        scope.disposed ||
        deployment?.revision !== target.revision
      )
        return;
    }
    const input = {
      deploymentId: target.deploymentId,
      expectedRevision: target.revision,
      idempotencyKey: ulid(),
    };
    await mutate(async () => {
      if (change === "enable") await trellis.deploymentsEnable(input).orThrow();
      else if (change === "disable")
        await trellis.deploymentsDisable(input).orThrow();
      else {
        await trellis.deploymentsRemove(input).orThrow();
        await goto(resolve("/admin/services"));
      }
    });
  }

  async function openSettings(instance: Instance): Promise<void> {
    settingsInstance = instance.instanceId;
    await tick();
    document
      .getElementById("settings")
      ?.scrollIntoView({ behavior: "smooth", block: "start" });
  }

  function ago(iso: string): string {
    const minutes = Math.floor((Date.now() - new Date(iso).getTime()) / 60000);
    if (!Number.isFinite(minutes)) return "—";
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    return hours < 24 ? `${hours} h ago` : `${Math.floor(hours / 24)} d ago`;
  }

  // A build as people know it: its reported version, or a short digest.
  function buildLabel(digest: string | undefined, version?: string): string {
    if (version) return version;
    return digest ? `Build ${digest.slice(0, 8)}` : "—";
  }

  onMount(() => () => scope.dispose());
</script>

{#snippet summary()}
  {#if deployment}
    <p
      class="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-base-content/75"
    >
      {#if health}<HealthStatus
          status={health.participant.effectiveStatus}
        /><span aria-hidden="true" class="text-base-content/40">·</span>{/if}
      <span class="tabular-nums"
        >{turnedOn} of {current.length}
        {current.length === 1 ? "instance" : "instances"} on</span
      >
      {#if health}<span aria-hidden="true" class="text-base-content/40">·</span
        ><span class="tabular-nums">{connected} connected</span>{/if}
      <span aria-hidden="true" class="text-base-content/40">·</span>
      <span>{installedText}</span>
    </p>
    <details class="mt-2 text-sm">
      <summary
        class="w-fit cursor-pointer text-base-content/65 hover:text-base-content"
        >Technical details</summary
      >
      <dl
        class="mt-2 grid max-w-3xl gap-x-4 gap-y-1 text-xs sm:grid-cols-[7rem_minmax(0,1fr)]"
      >
        <dt class="text-base-content/60">Service ID</dt>
        <dd class="flex items-center gap-1">
          <span class="trellis-identifier break-all"
            >{deployment.deploymentId}</span
          ><CopyButton
            value={deployment.deploymentId}
            label="Copy service ID"
          />
        </dd>
        <dt class="text-base-content/60">Contract</dt>
        <dd class="trellis-identifier break-all">{deployment.participantId}</dd>
        <dt class="text-base-content/60">Build digest</dt>
        <dd class="trellis-identifier break-all">
          {deployment.implementationDigest}
        </dd>
        <dt class="text-base-content/60">Revision</dt>
        <dd class="tabular-nums">{deployment.revision}</dd>
      </dl>
    </details>
  {/if}
{/snippet}

{#snippet meta()}
  {#if deployment}<StatusBadge
      label={deployment.state === "enabled" ? "Active" : "Disabled"}
      status={deployment.state === "enabled" ? "healthy" : "offline"}
    />{/if}
{/snippet}

<section class="max-w-5xl space-y-5">
  <PageToolbar
    eyebrow="Services"
    title={deployment
      ? humanizeName(deployment.title)
      : claim
        ? humanizeName(claim.title)
        : "Service"}
    description={deployment
      ? humanizeName(deployment.participantId)
      : undefined}
    {meta}
    {summary}
    >{#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load(deploymentId)}
        ><Icon name="refresh" size={16} />Refresh</button
      >{#if deployment}<ActionMenu
          widthClass="w-48"
          ariaLabel="Service actions"
          buttonBaseClass="btn btn-ghost btn-sm btn-square"
        >
          {#if deployment.state === "enabled"}
            <li>
              <button
                disabled={busy ||
                  uncertain ||
                  !canPerform(authority.authority, "deploymentsDisable")}
                onclick={() => void changeService("disable")}
                >Disable service</button
              >
            </li>
          {:else if deployment.state === "disabled"}
            <li>
              <button
                disabled={busy ||
                  uncertain ||
                  !canPerform(authority.authority, "deploymentsEnable")}
                onclick={() => void changeService("enable")}
                >Enable service</button
              >
            </li>
          {/if}
          <li>
            <button
              class="text-error"
              disabled={busy ||
                uncertain ||
                !canPerform(authority.authority, "deploymentsRemove")}
              onclick={() => void changeService("remove")}
              >Remove service</button
            >
          </li>
        </ActionMenu>{/if}{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if claim}
    <div class="trellis-section flex flex-wrap items-center gap-3 p-5">
      <span class="badge badge-neutral">Waiting for its service</span>
      <span class="text-sm text-base-content/65"
        >When it connects, approve what it asks for to add it.</span
      >
      {#if canPerform(authority.authority, "serviceClaimsRemove")}<button
          class="btn btn-outline btn-error btn-sm ml-auto"
          disabled={busy}
          onclick={() => void removeClaim()}>Remove</button
        >{/if}
    </div>
    {#if canPerform(authority.authority, "serviceUpdatesList")}<ServiceUpdates
        deploymentId={claim.claimId}
        focus={page.url.searchParams.get("update") ?? undefined}
        onapplied={() => void load(deploymentId)}
      />{/if}
    {#if canPerform(authority.authority, "configGet")}<ParticipantSettings
        claimId={claim.claimId}
      />{/if}
  {/if}
  {#if deployment && canPerform(authority.authority, "serviceUpdatesList")}<ServiceUpdates
      deploymentId={deployment.deploymentId}
      focus={page.url.searchParams.get("update") ?? undefined}
      onapplied={() => void load(deploymentId)}
      bind:updates
      bind:titles={capabilityTitles}
    />{/if}
  {#if loading && !deployment}<LoadingState
      label="Loading service"
    />{:else if deployment}
    <section class="trellis-section" aria-labelledby="instances-title">
      <div
        class="flex flex-wrap items-center justify-between gap-3 px-5 pt-5 pb-3"
      >
        <h2 id="instances-title" class="text-base font-semibold">Instances</h2>
        {#if deployment.state === "enabled" && canPerform(authority.authority, "serviceInstancesProvision")}<button
            class="btn btn-outline btn-sm"
            aria-expanded={adding}
            onclick={() => {
              adding = !adding;
              receipt = null;
            }}><Icon name="plus" size={16} />Add an instance</button
          >{/if}
      </div>
      {#if instanceError}<Notice variant="warning" class="mx-5 mb-3"
          >Some instances couldn't be loaded: {instanceError}</Notice
        >{/if}
      {#if adding}
        <form
          class="trellis-form border-t border-base-300 bg-base-200/40 px-5 py-4"
          aria-label="Add an instance"
          onsubmit={(event) => {
            event.preventDefault();
            void provision();
          }}
        >
          <label class="trellis-field"
            ><span class="trellis-field-label">Instance title</span><input
              class="input w-full"
              bind:value={title}
              required
            /></label
          >
          <details class="text-sm">
            <summary class="cursor-pointer text-base-content/70"
              >Use the instance's own key</summary
            >
            <label class="trellis-field mt-2"
              ><span class="trellis-field-label">Public key</span><input
                class="input w-full font-mono"
                bind:value={identityPublicKey}
              /><span class="trellis-field-help"
                >Leave empty to get a one-time setup key instead.</span
              ></label
            >
          </details>
          <div class="trellis-action-row">
            <button
              class="btn btn-ghost"
              type="button"
              onclick={() => (adding = false)}>Cancel</button
            >
            <button
              class="btn btn-primary"
              type="submit"
              disabled={busy || uncertain || !title.trim()}>Provision</button
            >
          </div>
        </form>
      {/if}
      {#if receipt}
        <div class="border-t border-base-300 bg-base-200/40 px-5 py-4">
          <Panel title="Provisioning receipt"
            ><p class="text-sm">
              Added {receipt.instance.principal.title || "a new instance"}.
            </p>
            {#if receipt.provisioningSecret}<p class="trellis-field-help mt-2">
                Give this one-time setup key to the new instance. You won't see
                it again.
              </p>
              <code
                class="mt-2 block break-all rounded-field bg-base-200 p-3 select-all"
                >{receipt.provisioningSecret}</code
              >{:else}<p class="trellis-field-help mt-2">
                It will connect with its own key; no setup key is needed.
              </p>{/if}</Panel
          >
        </div>
      {/if}
      <DataTable class="border-t border-base-300">
        <thead>
          <tr>
            <th class="trellis-primary">Instance</th>
            <th><span class="max-sm:sr-only">State</span></th>
            <th class="hidden sm:table-cell">Connection</th>
            <th class="hidden sm:table-cell">Build</th>
            <th class="hidden sm:table-cell">Added</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each listed as instance (instance.instanceId)}
            {@const identity = identities.get(instance.instanceId)}
            {@const report = running.get(instance.instanceId)}
            {@const build = buildLabel(
              report?.contractDigest,
              report?.latestSample.participant.version,
            )}
            <tr
              data-instance-id={instance.instanceId}
              class={instance.state === "removed"
                ? "text-base-content/55"
                : undefined}
            >
              <td class="trellis-primary">
                <span class="block font-medium">{identity?.name}</span>
                <span
                  class="flex flex-wrap gap-x-3 text-xs text-base-content/60"
                >
                  <span class="max-sm:hidden"
                    ><span class="trellis-identifier"
                      >{instance.instanceId}</span
                    ></span
                  >
                  {#if identity?.key}<span
                      >Key <span class="trellis-identifier">{identity.key}</span
                      ></span
                    >{/if}
                </span>
                <span class="block text-xs text-base-content/60 sm:hidden"
                  >{report
                    ? report.effectiveStatus === "offline"
                      ? `Last seen ${ago(report.observedAt)}`
                      : `Connected · seen ${ago(report.observedAt)}`
                    : "Never connected"} · {build}</span
                >
                {#if report && report.effectiveStatus !== "offline" && !instance.upToDate}<span
                    class="mt-0.5 flex items-center gap-1 text-xs text-warning"
                    ><Icon name="alert" size={12} />Runs an older build</span
                  >{:else if instance.state === "disabled" && !instance.upToDate}<span
                    class="mt-0.5 flex items-center gap-1 text-xs text-base-content/60"
                    >Holds an older build; it can't be turned on</span
                  >{/if}
              </td>
              <td>
                <StatusBadge
                  compact
                  label={instance.state === "active"
                    ? "On"
                    : instance.state === "removed"
                      ? "Removed"
                      : "Off"}
                  status={instance.state === "active" ? "healthy" : "offline"}
                />
              </td>
              <td class="hidden sm:table-cell">
                {#if report}
                  <span class="flex flex-col gap-0.5">
                    <HealthStatus status={report.effectiveStatus} />
                    <span class="text-xs text-base-content/60"
                      >Seen {ago(report.observedAt)}</span
                    >
                  </span>
                {:else}<span class="text-base-content/55">Never connected</span
                  >{/if}
              </td>
              <td class="hidden sm:table-cell">
                <span
                  class={report?.latestSample.participant.version
                    ? undefined
                    : "trellis-identifier"}
                  title={report?.contractDigest}>{build}</span
                >
              </td>
              <td class="hidden whitespace-nowrap sm:table-cell"
                >{formatDate(instance.principal.createdAt)}</td
              >
              <td class="text-right">
                {#if instance.state !== "removed"}
                  <ActionMenu
                    widthClass="w-48"
                    ariaLabel={`Actions for ${identity?.name ?? instance.instanceId}`}
                  >
                    {#if instance.state === "active"}
                      <li>
                        <button
                          disabled={busy ||
                            uncertain ||
                            !canPerform(
                              authority.authority,
                              "serviceInstancesDisable",
                            )}
                          onclick={() =>
                            void changeInstance(instance, "disable")}
                          >Turn off</button
                        >
                      </li>
                    {:else}
                      <li>
                        <button
                          disabled={busy ||
                            uncertain ||
                            deployment.state !== "enabled" ||
                            !instance.upToDate ||
                            !canPerform(
                              authority.authority,
                              "serviceInstancesEnable",
                            )}
                          title={!instance.upToDate
                            ? "It holds an older build than the service's installed one."
                            : deployment.state !== "enabled"
                              ? "Enable the service first."
                              : undefined}
                          onclick={() =>
                            void changeInstance(instance, "enable")}
                          >Turn on</button
                        >
                      </li>
                    {/if}
                    {#if settingsAvailable}
                      <li>
                        <button onclick={() => void openSettings(instance)}
                          >Settings</button
                        >
                      </li>
                    {/if}
                    <li>
                      <button
                        class="text-error"
                        disabled={busy ||
                          uncertain ||
                          !canPerform(
                            authority.authority,
                            "serviceInstancesRemove",
                          )}
                        onclick={() => void changeInstance(instance, "remove")}
                        >Remove</button
                      >
                    </li>
                  </ActionMenu>
                {/if}
              </td>
            </tr>
          {:else}
            <tr
              ><td colspan="6" class="text-base-content/60"
                >No instances yet. Add one, or let a service connect with a key
                you provisioned.</td
              ></tr
            >
          {/each}
        </tbody>
      </DataTable>
      {#if removed.length}
        <div class="border-t border-base-300 px-5 py-3">
          <button
            type="button"
            class="btn btn-ghost btn-sm"
            onclick={() => (showRemoved = !showRemoved)}
            >{showRemoved
              ? "Hide removed instances"
              : `Show ${removed.length} removed`}</button
          >
        </div>
      {/if}
    </section>

    {#if canPerform(authority.authority, "configGet")}<ParticipantSettings
        deploymentId={deployment.deploymentId}
        {instanceNames}
        bind:instanceId={settingsInstance}
        bind:available={settingsAvailable}
      />{/if}

    {#if canPerform(authority.authority, "serviceUpdatesList")}<ServiceUpdateHistory
        {updates}
        titles={capabilityTitles}
      />{/if}
  {/if}
</section>
<ConfirmationModal bind:this={confirmationModal} />
