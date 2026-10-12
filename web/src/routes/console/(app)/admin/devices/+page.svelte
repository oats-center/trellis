<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { resolve } from "$lib/console_paths";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import {
    captureIntent,
    classifyMutationError,
    type MutationIntent,
  } from "$lib/console/mutation.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { bulkExpectedCount, bulkTargetDetails, runBulk } from "$lib/bulk.ts";
  import { getTrellis } from "$lib/trellis";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import SelectableRecordButton from "$lib/components/SelectableRecordButton.svelte";
  import SelectionRail from "$lib/components/SelectionRail.svelte";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];
  type Instance = apis.auth.DevicesListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const deploymentScope = new RequestScope("device-deployments");
  const instanceScope = new RequestScope("device-instances");
  let deployments = $state.raw<Deployment[]>([]);
  let instances = $state.raw<Instance[]>([]);
  let selectedDeploymentId = $state("");
  let search = $state("");
  let loading = $state(true);
  let deploymentError = $state<string | null>(null);
  let instanceError = $state<string | null>(null);
  const selectedIds = new SvelteSet<string>();
  let busy = $state(false);
  let unknown = $state<string[]>([]);
  let bulkResult = $state<{ succeeded: number; failed: string[] } | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  const selectedDeployment = $derived(
    deployments.find((item) => item.deploymentId === selectedDeploymentId),
  );
  const selectedInstances = $derived(
    instances.filter((item) => item.deploymentId === selectedDeploymentId),
  );
  const filteredDeployments = $derived(
    deployments.filter((item) =>
      `${item.title} ${item.deploymentId}`
        .toLowerCase()
        .includes(search.toLowerCase()),
    ),
  );
  const candidates = $derived(
    selectedInstances.filter((item) => item.principal.enabled),
  );

  async function load(): Promise<void> {
    loading = true;
    const deploymentToken = deploymentScope.begin();
    const instanceToken = instanceScope.begin();
    const [listedDeployments, listedInstances] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis.deploymentsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.devicesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    if (deploymentScope.settle(deploymentToken)) {
      deployments = listedDeployments.items.filter(
        (item) => item.kind === "device",
      );
      deploymentError = listedDeployments.complete
        ? null
        : projectConsoleError(listedDeployments.error).message;
      if (!selectedDeploymentId && deployments.length)
        selectedDeploymentId = deployments[0].deploymentId;
      loading = false;
    }
    if (instanceScope.settle(instanceToken)) {
      instances = [...listedInstances.items];
      instanceError = listedInstances.complete
        ? null
        : projectConsoleError(listedInstances.error).message;
    }
  }

  async function disableSelected(): Promise<void> {
    if (
      busy ||
      unknown.length ||
      !canPerform(authority.authority, "devicesDisable")
    )
      return;
    const deploymentId = selectedDeploymentId;
    const intents = candidates
      .filter((item) => selectedIds.has(item.instanceId))
      .map((item) => {
        const key = ulid();
        return captureIntent<apis.auth.DevicesDisableInput>({
          operation: "devicesDisable",
          targetId: item.instanceId,
          label: item.instanceId,
          idempotencyKey: key,
          input: {
            instanceId: item.instanceId,
            expectedRevision: item.revision,
            idempotencyKey: key,
          },
          scope: { routeKey: deploymentId },
        });
      });
    if (!intents.length) return;
    const confirmed = await confirmationModal?.confirm({
      title: `Disable ${intents.length} device instances?`,
      message:
        "Selected device principals lose authentication eligibility. This does not report runtime health.",
      confirmLabel: "Disable selected",
      targetLabel: "Instances",
      targetName: `${intents.length} instances`,
      expectedValue: bulkExpectedCount(intents.length),
      details: bulkTargetDetails(intents.map((item) => item.label)),
    });
    if (
      !confirmed ||
      selectedDeploymentId !== deploymentId ||
      instanceScope.disposed
    )
      return;
    busy = true;
    const result = await runBulk(
      intents,
      async (intent: MutationIntent<apis.auth.DevicesDisableInput>) => {
        if (
          instanceScope.disposed ||
          selectedDeploymentId !== deploymentId ||
          !instances.some(
            (item) =>
              item.instanceId === intent.targetId &&
              item.revision === intent.input.expectedRevision &&
              item.principal.enabled,
          )
        )
          throw new Error("Target changed before dispatch; no disable sent.");
        await trellis.devicesDisable(intent.input).orThrow();
      },
      (cause) =>
        classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed",
    );
    if (instanceScope.disposed) return;
    busy = false;
    bulkResult = {
      succeeded: result.succeeded,
      failed: result.failed.map(
        (item) => `${item.target.label}: ${item.reason}`,
      ),
    };
    unknown = result.unknown.map((item) => item.target.label);
    selectedIds.clear();
    await load();
  }

  onMount(() => {
    void load();
    return () => {
      deploymentScope.dispose();
      instanceScope.dispose();
    };
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="Devices"
    description="Manage device deployments and provisioned identities. OAuth device approval is handled by the dedicated authorization Portal."
  >
    {#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        onclick={() => void load()}
        disabled={loading}>Refresh</button
      ><a
        class="btn btn-outline btn-sm"
        href={resolve("/admin/devices/profiles/new")}>Create deployment</a
      ><a
        class="btn btn-outline btn-sm"
        href={resolve("/admin/devices/instances/provision")}>Provision device</a
      >{/snippet}
  </PageToolbar>
  {#if deploymentError}<Notice variant="error"
      >Deployment catalog incomplete: {deploymentError}</Notice
    >{/if}
  {#if instanceError}<Notice variant="warning"
      >Device catalog incomplete: {instanceError}</Notice
    >{/if}
  {#if unknown.length}<Notice variant="warning"
      >Disable outcome unknown for {unknown.join(", ")}. Inspect these
      identities before another action.</Notice
    >{/if}
  {#if loading && !deployments.length}<Panel
      ><LoadingState label="Loading devices" /></Panel
    >{:else}
    <div
      class="grid min-h-[calc(100vh-12rem)] items-stretch gap-4 xl:grid-cols-[22rem_minmax(0,1fr)]"
    >
      <SelectionRail
        title="Deployments"
        eyebrow={`${deployments.length} loaded deployments`}
      >
        <label class="input input-bordered input-sm mb-3 flex items-center"
          ><input
            class="grow"
            bind:value={search}
            placeholder="Search deployment title or ID"
            aria-label="Search deployments"
          /></label
        >
        <div class="space-y-2">
          {#each filteredDeployments as deployment (deployment.deploymentId)}
            <SelectableRecordButton
              selected={selectedDeploymentId === deployment.deploymentId}
              onclick={() => {
                selectedDeploymentId = deployment.deploymentId;
                selectedIds.clear();
              }}
            >
              <div class="flex items-start justify-between gap-3">
                <div class="min-w-0">
                  <div class="truncate font-medium">{deployment.title}</div>
                  <div class="trellis-identifier truncate text-xs">
                    {deployment.deploymentId}
                  </div>
                  <div class="trellis-metadata">
                    Revision {deployment.revision}
                  </div>
                </div>
                <span
                  class="badge badge-sm {deployment.state === 'enabled'
                    ? 'badge-success'
                    : 'badge-neutral'}">{deployment.state}</span
                >
              </div>
            </SelectableRecordButton>
          {:else}<EmptyState
              title="No device deployments returned"
              description="Create a device deployment before provisioning identities."
            />{/each}
        </div>
      </SelectionRail>
      <Panel
        title={selectedDeployment?.title ?? "Select a deployment"}
        class="min-w-0"
      >
        {#if selectedDeployment}
          <div
            class="flex flex-wrap justify-between gap-3 border-b border-base-300 pb-3"
          >
            <div>
              <p class="trellis-identifier">
                {selectedDeployment.deploymentId}
              </p>
              <p class="trellis-field-help">
                {selectedDeployment.participantId}
              </p>
              <p class="trellis-metadata">
                Implementation {selectedDeployment.implementationDigest} · Revision
                {selectedDeployment.revision}
              </p>
            </div>
            {#if selectedDeployment.state === "enabled"}<a
                class="btn btn-error btn-outline btn-sm"
                href={resolve(
                  `/admin/devices/profiles/disable?deployment=${selectedDeployment.deploymentId}`,
                )}>Disable deployment</a
              >{/if}
          </div>
          {#if bulkResult}<BulkResult
              succeeded={bulkResult.succeeded}
              failed={bulkResult.failed}
              pastTense="instances disabled"
              onDismiss={() => {
                bulkResult = null;
              }}
            />{/if}
          {#if selectedIds.size}<BulkActionBar
              count={selectedIds.size}
              noun="instance"
              onClear={() => selectedIds.clear()}
              >{#snippet actions()}<button
                  class="btn btn-error btn-outline btn-sm"
                  disabled={busy ||
                    !!unknown.length ||
                    !canPerform(authority.authority, "devicesDisable")}
                  onclick={() => void disableSelected()}
                  >Disable selected</button
                >{/snippet}</BulkActionBar
            >{/if}
          <DataTable
            ><thead
              ><tr
                ><th>Select</th><th>Instance / Principal</th><th
                  >Identity public key</th
                ><th>Enabled</th><th>Revision</th><th>Actions</th></tr
              ></thead
            ><tbody>
              {#each selectedInstances as instance (instance.instanceId)}<tr
                  ><td
                    ><input
                      class="checkbox checkbox-xs"
                      type="checkbox"
                      aria-label={`Select ${instance.instanceId}`}
                      checked={selectedIds.has(instance.instanceId)}
                      disabled={!instance.principal.enabled || busy}
                      onchange={(event) => {
                        if (event.currentTarget.checked)
                          selectedIds.add(instance.instanceId);
                        else selectedIds.delete(instance.instanceId);
                      }}
                    /></td
                  ><td class="trellis-identifier"
                    >{instance.instanceId}
                    <div class="trellis-metadata">
                      {instance.principal.principalId}
                    </div></td
                  ><td class="trellis-identifier"
                    >{instance.identityPublicKey ?? "Not installed"}</td
                  ><td
                    ><span
                      class="badge badge-sm {instance.principal.enabled
                        ? 'badge-success'
                        : 'badge-neutral'}"
                      >{instance.principal.enabled
                        ? "Enabled"
                        : "Disabled"}</span
                    ></td
                  ><td>{instance.revision}</td><td
                    >{#if instance.principal.enabled}<a
                        class="btn btn-ghost btn-xs"
                        href={resolve(
                          `/admin/devices/instances/disable?instance=${instance.instanceId}`,
                        )}>Disable</a
                      >{/if}</td
                  ></tr
                >{:else}<tr
                  ><td colspan="6"
                    >No instances returned for this deployment.</td
                  ></tr
                >{/each}
            </tbody></DataTable
          >
        {:else}<EmptyState
            title="Deployment unavailable"
            description="Select a loaded deployment; a missing exact selection is not replaced silently."
          />{/if}
      </Panel>
    </div>
  {/if}
</section>
<ConfirmationModal bind:this={confirmationModal} />
