<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import { captureIntent, classifyMutationError, isIntentCurrent, type MutationIntent } from "$lib/console/mutation.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { decodeKnownJsonBytes } from "$lib/console/display_value.ts";
  import { SvelteSet } from "svelte/reactivity";
  import { onMount } from "svelte";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import SelectableRecordButton from "$lib/components/SelectableRecordButton.svelte";
  import SelectionRail from "$lib/components/SelectionRail.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { bulkExpectedCount, bulkTargetDetails, runBulk, toggleAll, toggleId } from "$lib/bulk.ts";
  import { getTrellis } from "$lib/trellis";

  type DeviceDeployment = apis.auth.DeploymentsListOutput["items"][number];
  type DeviceInstance = apis.auth.DevicesListOutput["items"][number];
  type Activation = apis.auth.DeviceUserAuthoritiesListOutput["items"][number];
  type Review = apis.auth.DeviceUserAuthoritiesReviewsListOutput["items"][number];
  type Tab = "instances" | "activations" | "reviews";
  type StatusVariant = "healthy" | "degraded" | "unhealthy" | "offline";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const tabs: Tab[] = ["instances", "activations", "reviews"];

  // Each collection loads and fails independently: a review-list failure must
  // not suppress device records that loaded, and vice versa.
  let loading = $state({ deployments: true, instances: true, activations: true, reviews: true });
  let error = $state<{
    deployments: string | null;
    instances: string | null;
    activations: string | null;
    reviews: string | null;
  }>({ deployments: null, instances: null, activations: null, reviews: null });
  let deployments = $state.raw<DeviceDeployment[]>([]);
  let instances = $state.raw<DeviceInstance[]>([]);
  let activations = $state.raw<Activation[]>([]);
  let reviews = $state.raw<Review[]>([]);

  let selectedDeploymentId = $state("");
  let activeTab = $state<Tab>("instances");
  let search = $state("");
  let selectedReviewId = $state<string | null>(null);

  const selectedDeployment = $derived(deployments.find((deployment) => deployment.deploymentId === selectedDeploymentId) ?? null);
  const instancesById = $derived.by(() => new Map(instances.map((instance) => [instance.instanceId, instance])));
  const selectedInstances = $derived(instances.filter((instance) => instance.deploymentId === selectedDeploymentId));
  const selectedActivations = $derived(activations.filter((activation) => activation.device.deploymentId === selectedDeploymentId));
  const selectedReviews = $derived(reviews.filter((review) => review.deploymentId === selectedDeploymentId));
  const selectedPendingReviews = $derived(selectedReviews.filter((review) => review.state === "pending"));
  const filteredDeployments = $derived.by(() => {
    const term = search.trim().toLowerCase();
    if (!term) return deployments;
    return deployments.filter((deployment) => deployment.deploymentId.toLowerCase().includes(term));
  });
  const selectedReview = $derived(selectedReviews.find((review) => review.reviewId === selectedReviewId) ?? selectedReviews[0] ?? null);
  const activeInstanceCount = $derived(selectedInstances.filter((instance) => instance.state === "active").length);
  const revokedActivationCount = $derived(selectedActivations.filter((activation) => activation.device.delegationState === "revoked").length);

  function syncSelectedDeployment(nextDeployments: DeviceDeployment[]): string {
    const nextDeploymentId = nextDeployments.some((deployment) => deployment.deploymentId === selectedDeploymentId)
      ? selectedDeploymentId
      : nextDeployments[0]?.deploymentId ?? "";
    if (nextDeploymentId !== selectedDeploymentId) selectedReviewId = null;
    selectedDeploymentId = nextDeploymentId;
    return nextDeploymentId;
  }

  function selectDeployment(deploymentId: string) {
    selectedDeploymentId = deploymentId;
    selectedReviewId = null;
  }

  function selectTab(tab: Tab) {
    activeTab = tab;
  }

  function instanceStatus(state: DeviceInstance["state"]): StatusVariant {
    if (state === "active") return "healthy";
    if (state === "pending") return "degraded";
    if (state === "revoked") return "unhealthy";
    return "offline";
  }

  function activationStatus(state: Activation["device"]["delegationState"]): StatusVariant {
    if (state === "active") return "healthy";
    if (state === "missing") return "degraded";
    return "unhealthy";
  }

  function reviewStatus(state: Review["state"]): StatusVariant {
    if (state === "approved") return "healthy";
    if (state === "pending") return "degraded";
    if (state === "rejected") return "unhealthy";
    return "offline";
  }


  function deploymentInstances(deploymentId: string): DeviceInstance[] {
    return instances.filter((instance) => instance.deploymentId === deploymentId);
  }

  function pendingReviewsForDeployment(deploymentId: string): number {
    return reviews.filter((review) => review.deploymentId === deploymentId && review.state === "pending").length;
  }

  function instanceRowKey(instance: DeviceInstance): string {
    return `${instance.instanceId}:${instance.createdAt}:${instance.identityPublicKey ?? ""}`;
  }

  const selectedInstanceIds = new SvelteSet<string>();
  let bulkBusy = $state(false);
  let bulkResult = $state<{ succeeded: number; failed: string[] } | null>(null);
  let uncertain = $state(false);
  let unknownInstances = $state<string[]>([]);
  let disposed = false;
  let confirmationModal: ConfirmationModal | undefined = $state();

  type DisableIntent = MutationIntent<apis.auth.DevicesDisableInput>;

  // Only active or pending identities are disable candidates; an already
  // disabled/revoked/unknown identity is not silently eligible.
  const disableableInstances = $derived(
    selectedInstances.filter((instance) =>
      instance.state === "active" || instance.state === "pending"
    ),
  );
  const selectableInstanceIds = $derived(disableableInstances.map((instance) => instance.instanceId));

  async function disableInstances(intents: DisableIntent[]) {
    bulkBusy = true;
    bulkResult = null;
    const outcome = await runBulk(intents, async (intent) => {
      if (
        disposed || !isIntentCurrent(intent, {

          routeKey: "/admin/devices",
        }) || !instances.some((instance) =>

          instance.instanceId === intent.targetId && instance.version === intent.input.expectedVersion &&

          (instance.state === "active" || instance.state === "pending")
        )
      ) {
        throw new Error("Target or authority changed before dispatch; no disable sent.");
      }
      await trellis.devicesDisable(intent.input).orThrow();
    }, (cause) => classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed");
    if (disposed) { bulkBusy = false; return; }
    for (const intent of intents) selectedInstanceIds.delete(intent.targetId);
    bulkResult = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((failure) => `${failure.target.label}: ${failure.reason}`),
    };
    if (outcome.unknown.length > 0) {
      uncertain = true;
      unknownInstances = outcome.unknown.map((item) => item.target.label);
    }
    bulkBusy = false;
    void load();
  }

  async function requestBulkDisable() {
    if (bulkBusy || uncertain) return;
    const targets = disableableInstances.filter((instance) => selectedInstanceIds.has(instance.instanceId));
    if (targets.length === 0) return;
    const intents = targets.map((instance) => {
      const idempotencyKey = ulid();
      return captureIntent<apis.auth.DevicesDisableInput>({
        operation: "devicesDisable",
        input: {
          expectedVersion: instance.version,
          idempotencyKey,
          instanceId: instance.instanceId,
          reason: null,
        },
        label: instance.instanceId,
        targetId: instance.instanceId,
        scope: {
          routeKey: "/admin/devices",
        },
        idempotencyKey,
      });
    });
    const confirmed = await confirmationModal?.confirm({
      title: `Disable ${targets.length} device instance${targets.length === 1 ? "" : "s"}?`,
      message: "Selected device instances stop operating. Each must complete activation again before use.",
      confirmLabel: `Disable ${targets.length}`,
      targetLabel: "Instances",
      targetName: `${targets.length} instances`,
      expectedValue: bulkExpectedCount(targets.length),
      details: bulkTargetDetails(intents.map((intent) => intent.label)),
    });
    if (
      !confirmed || disposed || uncertain || intents.some((intent) =>

        !isIntentCurrent(intent, {
          routeKey: "/admin/devices",
        })
      )
    ) return;
    await disableInstances(intents);
  }

  function activationRowKey(activation: Activation): string {
    return `${activation.device.instanceId}:${activation.device.updatedAt}`;
  }

  /** Decodes the deployment reviewMode byte payload; unknown values stay visible. */
  function reviewModeLabel(reviewMode: Uint8Array | null | undefined): string {
    if (reviewMode === null || reviewMode === undefined) return "none";
    const decoded = decodeKnownJsonBytes(reviewMode);
    if (!decoded.ok) return "unknown";
    return typeof decoded.value === "string" ? decoded.value : "unknown";
  }

  function tabLabel(tab: Tab): string {
    return tab[0].toUpperCase() + tab.slice(1);
  }

  function tabId(tab: Tab): string {
    return `device-detail-tab-${tab}`;
  }

  function tabPanelId(tab: Tab): string {
    return `device-detail-panel-${tab}`;
  }

  async function load() {
    loading = { deployments: true, instances: true, activations: true, reviews: true };
    error = { deployments: null, instances: null, activations: null, reviews: null };
    const page = { limit: TABLE_PAGE_LIMIT };
    const [deploymentsResult, instancesResult, activationsResult, reviewsResult] = await Promise.allSettled([
      trellis.deploymentsList({ kind: "device", page }).take(),
      trellis.devicesList({ page }).take(),
      trellis.deviceUserAuthoritiesList({ page }).take(),
      trellis.deviceUserAuthoritiesReviewsList({ page }).take(),
    ]);

    if (deploymentsResult.status === "fulfilled" && !isErr(deploymentsResult.value)) {
      deployments = (deploymentsResult.value.items ?? []).filter(
        (deployment): deployment is DeviceDeployment => deployment.kind === "device",
      );
      syncSelectedDeployment(deployments);
    } else {
      error.deployments = errorMessage(
        deploymentsResult.status === "rejected" ? deploymentsResult.reason : deploymentsResult.value,
      );
    }

    if (instancesResult.status === "fulfilled" && !isErr(instancesResult.value)) {
      instances = instancesResult.value.items ?? [];
    } else {
      error.instances = errorMessage(
        instancesResult.status === "rejected" ? instancesResult.reason : instancesResult.value,
      );
    }

    if (activationsResult.status === "fulfilled" && !isErr(activationsResult.value)) {
      activations = activationsResult.value.items ?? [];
    } else {
      error.activations = errorMessage(
        activationsResult.status === "rejected" ? activationsResult.reason : activationsResult.value,
      );
    }

    if (reviewsResult.status === "fulfilled" && !isErr(reviewsResult.value)) {
      reviews = reviewsResult.value.items ?? [];
      if (selectedReviewId && !reviews.some((review) => review.reviewId === selectedReviewId)) {
        selectedReviewId = null;
      }
    } else {
      error.reviews = errorMessage(
        reviewsResult.status === "rejected" ? reviewsResult.reason : reviewsResult.value,
      );
    }

    loading = { deployments: false, instances: false, activations: false, reviews: false };
  }

  onMount(() => {
    void load();
    return () => { disposed = true; };
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="Devices"
    description="Manage device deployments, provisioned identities, activation state, and review decisions from one operator surface."
  >
    {#snippet actions()}
      <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading.deployments}>Refresh</button>
      <a class="btn btn-outline btn-sm" href={resolve("/admin/devices/profiles/new")}>Create deployment</a>
      <a class="btn btn-outline btn-sm" href={resolve("/admin/devices/instances/provision")}>Provision device</a>
    {/snippet}
  </PageToolbar>

  {#if error.deployments}<Notice variant="error">Deployments: {error.deployments}</Notice>{/if}
  {#if error.instances}<Notice variant="warning">Device instances: {error.instances}</Notice>{/if}
  {#if error.activations}<Notice variant="warning">Activations: {error.activations}</Notice>{/if}
  {#if error.reviews}<Notice variant="warning">Reviews: {error.reviews}</Notice>{/if}

  {#if unknownInstances.length > 0}
    <Notice variant="warning">
      Disable outcome unknown for {unknownInstances.join(", ")}. The instances may already be disabled. Inspect the list and reload before any further action.
    </Notice>
  {/if}

  {#if loading.deployments && deployments.length === 0}
    <Panel><LoadingState label="Loading devices" /></Panel>
  {:else}
    <div class="grid min-h-[calc(100vh-12rem)] items-stretch gap-4 xl:grid-cols-[22rem_minmax(0,1fr)]">
      <SelectionRail title="Deployments" eyebrow={`${deployments.length} deployment${deployments.length === 1 ? "" : "s"}`}>
        <div class="mb-3">
          <label class="input input-bordered input-sm flex items-center gap-2">
            <Icon name="search" size={14} class="text-base-content/50" />
            <input bind:value={search} class="grow" placeholder="Search ID or review mode" aria-label="Search deployments" />
          </label>
        </div>

        {#if deployments.length === 0}
          <EmptyState title="No device deployments" description="Create a deployment before provisioning device identities." />
        {:else}
          <div class="space-y-2">
            {#each filteredDeployments as deployment (deployment.deploymentId)}
              {@const deploymentDeviceInstances = deploymentInstances(deployment.deploymentId)}
              {@const activeDevices = deploymentDeviceInstances.filter((instance) => instance.state === "active")}
              {@const pendingReviewCount = pendingReviewsForDeployment(deployment.deploymentId)}
              <SelectableRecordButton
                selected={selectedDeploymentId === deployment.deploymentId}
                onclick={() => selectDeployment(deployment.deploymentId)}
              >
                <div class="flex items-start justify-between gap-3">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2">
                      <span class={["h-2.5 w-2.5 rounded-full", deployment.state === "active" ? "bg-success" : "bg-base-content/30"]}></span>
                      <span class="trellis-identifier truncate font-medium">{deployment.deploymentId}</span>
                    </div>
                    <div class="mt-1 text-xs text-base-content/60">{activeDevices.length}/{deploymentDeviceInstances.length} activated instances</div>
                    <div class="mt-1 flex flex-wrap gap-1">
                      <span class="badge badge-outline badge-xs">review {reviewModeLabel(deployment.reviewMode)}</span>
                      <span class="badge badge-outline badge-xs">delegation {deployment.requiresDeviceDelegation ? "required" : "none"}</span>
                      {#if pendingReviewCount > 0}<span class="badge badge-warning badge-xs">{pendingReviewCount} review</span>{/if}
                    </div>
                  </div>
                  <span class={["badge badge-sm", deployment.state === "active" ? "badge-success" : "badge-neutral"]}>{deployment.state}</span>
                </div>
              </SelectableRecordButton>
            {:else}
              <EmptyState title="No matches" description="Try a different deployment ID or review mode." class="py-4" />
            {/each}
          </div>
        {/if}

        {#snippet footer()}
          <span>{deployments.filter((deployment) => deployment.state === "disabled").length} disabled / archived</span>
        {/snippet}
      </SelectionRail>

      <div class="flex min-w-0 flex-col gap-4">
        {#if !selectedDeployment}
          <Panel><EmptyState title="Select a deployment" description="Choose a device deployment from the left rail to inspect instances, activations, and reviews." /></Panel>
        {:else}
          <Panel class="flex min-w-0 flex-1 flex-col [&>.trellis-section-body]:flex-1">
            <div class="flex flex-wrap items-start justify-between gap-3 border-b border-base-300 pb-3">
              <div class="flex min-w-0 items-start gap-3">
                <div class="rounded-box bg-primary/10 p-2.5 text-primary"><Icon name="phone" size={22} /></div>
                <div class="min-w-0">
                  <div class="flex flex-wrap items-center gap-2">
                    <h2 class="trellis-identifier truncate text-lg font-semibold">{selectedDeployment.deploymentId}</h2>
                    <StatusBadge label={selectedDeployment.state} status={selectedDeployment.state === "active" ? "healthy" : "offline"} />
                  </div>
                  <div class="mt-1 flex flex-wrap gap-1 text-sm text-base-content/60">
                    <span>Review: <span class="badge badge-outline badge-sm">{reviewModeLabel(selectedDeployment.reviewMode)}</span></span>
                    <span>Delegation: <span class="badge badge-outline badge-sm">{selectedDeployment.requiresDeviceDelegation ? "required" : "none"}</span></span>
                  </div>
                </div>
              </div>
              <div class="flex flex-wrap gap-2">
                {#if selectedDeployment.state !== "disabled"}
                  <a class="btn btn-error btn-outline btn-sm" href={resolve(`/admin/devices/profiles/disable?deployment=${encodeURIComponent(selectedDeployment.deploymentId)}`)}>Disable deployment</a>
                {/if}
              </div>
            </div>

            <div class="mt-3 flex flex-wrap items-center gap-2 text-sm">
              <span class="badge badge-outline badge-sm">{activeInstanceCount}/{selectedInstances.length} activated instances</span>
              <span class="badge badge-outline badge-sm">{selectedPendingReviews.length} pending review{selectedPendingReviews.length === 1 ? "" : "s"}</span>
              <span class="badge badge-outline badge-sm">{selectedActivations.length} activation{selectedActivations.length === 1 ? "" : "s"}</span>
              <span class="badge badge-outline badge-sm">{revokedActivationCount} revoked</span>
            </div>

            {#if selectedPendingReviews.length > 0}
              <div class="mt-3 rounded-box border border-warning/30 bg-warning/10 px-3 py-2 text-sm">
                <div class="flex flex-wrap items-center justify-between gap-2">
                  <div>
                    <div class="font-medium">Activation review required</div>
                    <div class="mt-1 text-xs text-base-content/70">Pending device activations need an approve or reject decision.</div>
                  </div>
                  <button type="button" class="btn btn-ghost btn-xs" onclick={() => selectTab("reviews")}>{selectedPendingReviews.length} pending review{selectedPendingReviews.length === 1 ? "" : "s"}</button>
                </div>
              </div>
            {/if}


            <div class="tabs tabs-box tabs-sm mt-4 w-fit bg-base-200/70 p-1" role="tablist" aria-label="Deployment detail sections">
              {#each tabs as tab (tab)}
                <button type="button" id={tabId(tab)} role="tab" aria-selected={activeTab === tab} aria-controls={tabPanelId(tab)} class={["tab rounded-field px-4", activeTab === tab && "tab-active bg-base-100 shadow-sm"]} onclick={() => selectTab(tab)}>{tabLabel(tab)}</button>
              {/each}
            </div>

            <div id={tabPanelId(activeTab)} class="mt-4 flex-1" role="tabpanel" aria-labelledby={tabId(activeTab)}>
              {#if activeTab === "instances"}
                {#if bulkResult}
                  <BulkResult
                    succeeded={bulkResult.succeeded}
                    failed={bulkResult.failed}
                    pastTense="instances disabled"
                    onDismiss={() => { bulkResult = null; }}
                  />
                {:else if selectedInstanceIds.size > 0}
                  <BulkActionBar count={selectedInstanceIds.size} noun="instance" onClear={() => selectedInstanceIds.clear()}>
                    {#snippet actions()}
                      <button class="btn btn-error btn-outline btn-sm" disabled={bulkBusy || uncertain} onclick={() => void requestBulkDisable()}>
                        {bulkBusy ? "Disabling…" : "Disable selected"}
                      </button>
                    {/snippet}
                  </BulkActionBar>
                {/if}
                {#if selectedInstances.length === 0}
                  <EmptyState title="No device instances" description="Provisioned device identities for this deployment appear here." />
                {:else}
                  <DataTable>
                      <thead><tr>
                        <th>
                          <span class="sr-only">Select all instances</span>
                          <input
                            type="checkbox"
                            class="checkbox checkbox-xs"
                            aria-label="Select all instances"
                            disabled={bulkBusy || selectableInstanceIds.length === 0}
                            checked={selectableInstanceIds.length > 0 && selectableInstanceIds.every((id) => selectedInstanceIds.has(id))}
                            indeterminate={selectableInstanceIds.some((id) => selectedInstanceIds.has(id)) && !selectableInstanceIds.every((id) => selectedInstanceIds.has(id))}
                            onchange={() => toggleAll(selectedInstanceIds, selectableInstanceIds)}
                          />
                        </th>
                        <th>Instance</th><th>Identity key</th><th>State</th><th>Created</th><th>Actions</th></tr></thead>
                      <tbody>
                        {#each selectedInstances as instance (instanceRowKey(instance))}
                          <tr>
                            <td>
                              {#if instance.state !== "disabled"}
                                <input
                                  type="checkbox"
                                  class="checkbox checkbox-xs"
                                  aria-label={`Select instance {instance.instanceId}`}
                                  disabled={bulkBusy}
                                  checked={selectedInstanceIds.has(instance.instanceId)}
                                  onchange={() => toggleId(selectedInstanceIds, instance.instanceId)}
                                />
                              {:else}
                                <span class="text-xs text-base-content/50">—</span>
                              {/if}
                            </td>
                            <td class="trellis-identifier font-medium">{instance.instanceId}</td>
                            <td class="trellis-identifier text-base-content/60">{instance.identityPublicKey ?? "—"}</td>
                            <td><StatusBadge label={instance.state} status={instanceStatus(instance.state)} /></td>
                            <td class="text-base-content/60">{formatDate(instance.createdAt)}</td>
                            <td>
                              {#if instance.state === "disabled"}
                                <span class="text-xs text-base-content/40">—</span>
                              {:else}
                                <a class="btn btn-error btn-outline btn-xs" href={resolve(`/admin/devices/instances/disable?instance=${encodeURIComponent(instance.instanceId)}`)}>Disable</a>
                              {/if}
                            </td>
                          </tr>
                        {/each}
                      </tbody>
                  </DataTable>
                {/if}
              {:else if activeTab === "activations"}
                {#if selectedActivations.length === 0}
                  <EmptyState title="No device activations" description="Activation records for this deployment appear here." />
                {:else}
                  <DataTable>
                      <thead><tr><th>Instance</th><th>Principal</th><th>Delegation</th><th>Created</th><th>Revoked</th><th>Actions</th></tr></thead>
                      <tbody>
                        {#each selectedActivations as activation (activationRowKey(activation))}
                          <tr>
                            <td><div class="trellis-identifier font-medium">{activation.device.instanceId}</div></td>
                            <td class="trellis-identifier text-base-content/60">{activation.device.principalId}</td>
                            <td><StatusBadge label={activation.device.delegationState} status={activationStatus(activation.device.delegationState)} /></td>
                            <td class="text-base-content/60">{formatDate(activation.device.createdAt)}</td>
                            <td class="text-base-content/60">{activation.device.delegationState === "revoked" ? formatDate(activation.device.updatedAt) : "—"}</td>
                            <td>
                              {#if activation.device.delegationState !== "active"}
                                <span class="text-xs text-base-content/40">—</span>
                              {:else}
                                <a class="btn btn-error btn-outline btn-xs" href={resolve(`/admin/devices/activations/revoke?instance=${encodeURIComponent(activation.device.instanceId)}`)}>Revoke</a>
                              {/if}
                            </td>
                          </tr>
                        {/each}
                      </tbody>
                  </DataTable>
                {/if}
              {:else if activeTab === "reviews"}
                <div class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]">
                  <div class="min-w-0">
                    {#if selectedReviews.length === 0}
                      <EmptyState title="No device reviews" description="Activation reviews for this deployment appear here." />
                    {:else}
                      <DataTable>
                          <thead><tr><th>Review</th><th>Instance</th><th>State</th><th>Requested</th><th>Actions</th></tr></thead>
                          <tbody>
                            {#each selectedReviews as review (review.reviewId)}
                              <tr class={{ "bg-base-200/60": selectedReview?.reviewId === review.reviewId }}>
                                <td><button class="trellis-identifier text-left hover:underline" onclick={() => (selectedReviewId = review.reviewId)}>{review.reviewId}</button></td>
                                <td><div class="trellis-identifier">{review.instanceId}</div><div class="trellis-identifier text-xs text-base-content/60">{review.devicePrincipalId}</div></td>
                                <td><StatusBadge label={review.state} status={reviewStatus(review.state)} /></td>
                                <td class="text-base-content/60">{formatDate(review.requestedAt)}</td>
                                <td>
                                  {#if review.state === "pending"}
                                    <a class="btn btn-ghost btn-xs" href={resolve(`/admin/devices/reviews/decide?review=${encodeURIComponent(review.reviewId)}`)}>Decide</a>
                                  {:else}
                                    <span class="text-xs text-base-content/40">—</span>
                                  {/if}
                                </td>
                              </tr>
                            {/each}
                          </tbody>
                      </DataTable>
                    {/if}
                  </div>
                  <div class="rounded-box border border-base-300 bg-base-200/30 p-3">
                    {#if selectedReview}
                      <div class="space-y-3 text-sm">
                        <div class="flex items-center justify-between gap-3">
                          <span class="trellis-identifier font-medium">{selectedReview.reviewId}</span>
                          <StatusBadge label={selectedReview.state} status={reviewStatus(selectedReview.state)} />
                        </div>
                        <div>
                          <p class="text-[0.65rem] font-semibold uppercase tracking-wider text-base-content/50">Instance</p>
                          <p class="trellis-identifier">{selectedReview.instanceId}</p>
                          <p class="trellis-identifier text-base-content/60">{selectedReview.devicePrincipalId}</p>
                        </div>
                        <div class="grid grid-cols-2 gap-2 text-xs">
                          <div><span class="text-base-content/50">Requested</span><div>{formatDate(selectedReview.requestedAt)}</div></div>
                          <div><span class="text-base-content/50">Decided</span><div>{selectedReview.decidedAt ? formatDate(selectedReview.decidedAt) : "—"}</div></div>
                          <div class="col-span-2"><span class="text-base-content/50">Reason</span><div>{selectedReview.reason ?? "—"}</div></div>
                        </div>
                        {#if selectedReview.state === "pending"}
                          <a class="btn btn-outline btn-sm w-full" href={resolve(`/admin/devices/reviews/decide?review=${encodeURIComponent(selectedReview.reviewId)}`)}>Decide review</a>
                        {/if}
                      </div>
                    {:else}
                      <EmptyState title="Select a review" description="Choose a review to inspect activation metadata." class="py-4" />
                    {/if}
                  </div>
                </div>
              {/if}
            </div>
          </Panel>
        {/if}
      </div>
    </div>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
