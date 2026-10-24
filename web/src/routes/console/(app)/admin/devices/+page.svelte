<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onMount } from "svelte";
  import { SvelteSet } from "svelte/reactivity";
  import { page } from "$app/state";
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
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import { formatDate, humanizeName } from "$lib/format";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];
  type Instance = apis.auth.DevicesListOutput["items"][number];
  type Review = apis.auth.DevicesReviewsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const deploymentScope = new RequestScope("device-deployments");
  const instanceScope = new RequestScope("device-instances");
  let deployments = $state.raw<Deployment[]>([]);
  let instances = $state.raw<Instance[]>([]);
  let selectedDeploymentId = $state("");
  let view = $state<"devices" | "requests">(
    page.url.searchParams.get("view") === "requests" ? "requests" : "devices",
  );
  let reviews = $state.raw<Review[]>([]);
  let reviewsError = $state<string | null>(null);
  let selectedReviewId = $state("");
  let code = $state("");
  let deciding = $state<"approved" | "declined" | null>(null);
  let decisionError = $state<string | null>(null);
  const pending = $derived(reviews.filter((item) => item.state === "pending"));
  const selectedReview = $derived(
    pending.find((item) => item.reviewId === selectedReviewId) ?? pending[0],
  );
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
  const typeNames = $derived(
    new Map(
      deployments.map((item) => [item.deploymentId, humanizeName(item.title)]),
    ),
  );
  const selectedInstances = $derived(
    instances.filter(
      (item) =>
        (!selectedDeploymentId || item.deploymentId === selectedDeploymentId) &&
        `${deviceName(item)} ${typeNames.get(item.deploymentId) ?? ""}`
          .toLowerCase()
          .includes(search.trim().toLowerCase()),
    ),
  );

  // Devices have no authored name; show the principal title when it is one,
  // otherwise a short form of the ID people can match against the device.
  function deviceName(item: {
    principal: { title: string };
    instanceId: string;
  }) {
    return item.principal.title && item.principal.title !== item.instanceId
      ? item.principal.title
      : `Device ${item.instanceId.slice(-6)}`;
  }

  function reviewName(item: Review): string {
    const instance = instances.find(
      (candidate) => candidate.instanceId === item.instanceId,
    );
    return instance
      ? deviceName(instance)
      : `Device ${item.instanceId.slice(-6)}`;
  }

  async function loadReviews(): Promise<void> {
    const listed = await traverseAll(async (page) => {
      const response = await trellis.devicesReviewsList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    reviews = [...listed.items];
    reviewsError = listed.complete
      ? null
      : projectConsoleError(listed.error).message;
  }

  async function decide(decision: "approved" | "declined"): Promise<void> {
    const review = selectedReview;
    if (!review || deciding) return;
    deciding = decision;
    decisionError = null;
    try {
      await trellis
        .devicesReviewsDecide({
          reviewId: review.reviewId,
          confirmationCode: code.trim().toUpperCase(),
          decision,
          expectedRevision: review.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      code = "";
      selectedReviewId = "";
      await Promise.all([loadReviews(), load()]);
    } catch (cause) {
      decisionError = projectConsoleError(cause).message;
    } finally {
      deciding = null;
    }
  }
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
          label: deviceName(item),
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
      title: `Turn off ${intents.length} device${intents.length === 1 ? "" : "s"}?`,
      message: "They will be disconnected and can't reconnect until turned on.",
      confirmLabel: "Turn off",
      targetLabel: "Devices",
      targetName: `${intents.length} device${intents.length === 1 ? "" : "s"}`,
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
    void loadReviews().catch((cause: unknown) => {
      reviewsError = projectConsoleError(cause).message;
    });
    return () => {
      deploymentScope.dispose();
      instanceScope.dispose();
    };
  });
</script>

<section class="space-y-5">
  <PageToolbar title="Devices" description="Devices connected to Trellis.">
    {#snippet actions()}
      <a class="btn btn-outline" href={resolve("/admin/devices/profiles/new")}
        >New device type</a
      >
      <a
        class="btn btn-primary"
        href={resolve("/admin/devices/instances/provision")}
        ><Icon name="plus" size={18} />Add device</a
      >
    {/snippet}
  </PageToolbar>

  {#if deploymentError}<Notice variant="error">{deploymentError}</Notice>{/if}
  {#if instanceError}<Notice variant="warning">{instanceError}</Notice>{/if}
  {#if unknown.length}<Notice variant="warning"
      >We couldn't confirm whether {unknown.join(", ")} turned off. Check them before
      trying again.</Notice
    >{/if}

  <div class="trellis-segment w-fit" role="group" aria-label="View">
    <button
      type="button"
      class={{ active: view === "devices" }}
      aria-pressed={view === "devices"}
      onclick={() => (view = "devices")}>Devices</button
    >
    <button
      type="button"
      class={{ active: view === "requests" }}
      aria-pressed={view === "requests"}
      onclick={() => (view = "requests")}
      >Requests{#if pending.length}<span
          class="badge badge-warning badge-xs ml-1.5">{pending.length}</span
        >{/if}</button
    >
  </div>

  {#if view === "requests"}
    {#if reviewsError}<Notice variant="error">{reviewsError}</Notice>{/if}
    {#if !pending.length}
      <EmptyState
        title="No requests waiting"
        description="When a new device asks to connect, it shows up here for approval."
      />
    {:else}
      <div class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
        <div class="trellis-section">
          <h2 class="px-5 pt-5 text-base font-semibold">
            Waiting for approval
            <span class="badge badge-sm badge-neutral ml-1"
              >{pending.length}</span
            >
          </h2>
          <ul class="mt-3 divide-y divide-base-300 border-t border-base-300">
            {#each pending as review (review.reviewId)}
              <li>
                <button
                  type="button"
                  class={[
                    "flex w-full items-center gap-3 px-5 py-3 text-left hover:bg-base-200/60",
                    selectedReview?.reviewId === review.reviewId &&
                      "bg-primary/10",
                  ]}
                  onclick={() => {
                    selectedReviewId = review.reviewId;
                    code = "";
                    decisionError = null;
                  }}
                >
                  <span
                    class="grid h-9 w-9 place-items-center rounded-full bg-base-200 text-base-content/70"
                    ><Icon name="laptop" size={18} /></span
                  >
                  <span class="min-w-0 flex-1">
                    <span class="block truncate font-medium"
                      >{reviewName(review)}</span
                    >
                    <span class="block truncate text-xs text-base-content/60"
                      >{typeNames.get(review.deploymentId) ??
                        humanizeName(review.participantId)}</span
                    >
                  </span>
                  <span class="badge badge-warning badge-sm">Pending</span>
                </button>
              </li>
            {/each}
          </ul>
        </div>

        {#if selectedReview}
          <div class="trellis-section p-5">
            <div class="flex items-center gap-4">
              <span
                class="grid h-14 w-14 place-items-center rounded-box bg-base-200 text-base-content/70"
                ><Icon name="laptop" size={26} /></span
              >
              <div>
                <h2 class="text-xl font-semibold">
                  {reviewName(selectedReview)}
                </h2>
                <p class="text-sm text-base-content/65">
                  {typeNames.get(selectedReview.deploymentId) ??
                    humanizeName(selectedReview.participantId)}
                </p>
              </div>
            </div>
            <dl
              class="mt-5 grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm"
            >
              <dt class="text-base-content/60">Device type</dt>
              <dd>
                {typeNames.get(selectedReview.deploymentId) ?? "Unknown"}
              </dd>
              <dt class="text-base-content/60">Software</dt>
              <dd>{humanizeName(selectedReview.participantId)}</dd>
              <dt class="text-base-content/60">Request expires</dt>
              <dd>{formatDate(selectedReview.expiresAt)}</dd>
              {#if selectedReview.companionRequired}
                <dt class="text-base-content/60">Companion app</dt>
                <dd>Required</dd>
              {/if}
            </dl>

            <form
              class="mt-6 border-t border-base-300 pt-5"
              onsubmit={(event) => {
                event.preventDefault();
                void decide("approved");
              }}
            >
              <label class="trellis-field">
                <span class="trellis-field-label">Code shown on the device</span
                >
                <input
                  class="input w-48 font-mono text-lg tracking-[0.2em] uppercase"
                  bind:value={code}
                  maxlength="8"
                  autocomplete="off"
                  spellcheck="false"
                  required
                />
                <span class="trellis-field-help"
                  >Check the device in front of you. Only approve if the codes
                  match.</span
                >
              </label>
              {#if decisionError}<Notice variant="error" class="mt-3"
                  >{decisionError}</Notice
                >{/if}
              <div class="mt-5 flex gap-3">
                <button
                  type="button"
                  class="btn btn-error btn-outline flex-1"
                  disabled={deciding !== null || code.trim().length !== 8}
                  onclick={() => void decide("declined")}
                  ><Icon name="close" size={16} />Reject</button
                >
                <button
                  type="submit"
                  class="btn btn-primary flex-1"
                  disabled={deciding !== null || code.trim().length !== 8}
                  ><Icon name="check" size={16} />Approve</button
                >
              </div>
            </form>
          </div>
        {/if}
      </div>
    {/if}
  {:else if loading && !deployments.length}
    <LoadingState label="Loading devices" />
  {:else}
    <div class="trellis-section">
      <div class="flex flex-wrap items-center gap-3 p-4">
        <label
          class="input flex w-full max-w-md items-center gap-2 bg-base-100 sm:w-auto sm:flex-1"
        >
          <Icon name="search" size={16} class="text-base-content/50" />
          <input
            class="grow"
            bind:value={search}
            placeholder="Search devices"
            aria-label="Search devices"
          />
        </label>
        <select
          class="select w-auto bg-base-100"
          aria-label="Device type"
          bind:value={selectedDeploymentId}
          onchange={() => selectedIds.clear()}
        >
          <option value="">All device types</option>
          {#each deployments as deployment (deployment.deploymentId)}
            <option value={deployment.deploymentId}
              >{humanizeName(deployment.title)}{deployment.state === "enabled"
                ? ""
                : " (off)"}</option
            >
          {/each}
        </select>
        {#if selectedDeployment?.state === "enabled"}
          <a
            class="btn btn-ghost btn-sm text-error"
            href={resolve(
              `/admin/devices/profiles/disable?deployment=${selectedDeployment.deploymentId}`,
            )}>Turn off this type</a
          >
        {/if}
      </div>

      {#if bulkResult}<div class="px-4">
          <BulkResult
            succeeded={bulkResult.succeeded}
            failed={bulkResult.failed}
            pastTense="devices turned off"
            onDismiss={() => {
              bulkResult = null;
            }}
          />
        </div>{/if}
      {#if selectedIds.size}<div class="px-4">
          <BulkActionBar
            count={selectedIds.size}
            noun="device"
            onClear={() => selectedIds.clear()}
            >{#snippet actions()}<button
                class="btn btn-error btn-outline btn-sm"
                disabled={busy ||
                  !!unknown.length ||
                  !canPerform(authority.authority, "devicesDisable")}
                onclick={() => void disableSelected()}>Turn off</button
              >{/snippet}</BulkActionBar
          >
        </div>{/if}

      {#if !selectedInstances.length}
        <EmptyState
          class="m-4"
          title={instances.length ? "No devices match" : "No devices yet"}
          description={instances.length
            ? "Try a different search or device type."
            : "Add a device, or approve one under Requests."}
        />
      {:else}
        <DataTable>
          <thead>
            <tr>
              <th class="w-10"><span class="sr-only">Select</span></th>
              <th>Device</th>
              <th>Type</th>
              <th>Status</th>
              <th>Added</th>
              <th><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each selectedInstances as instance (instance.instanceId)}
              <tr>
                <td>
                  <input
                    class="checkbox checkbox-sm"
                    type="checkbox"
                    aria-label={`Select ${deviceName(instance)}`}
                    checked={selectedIds.has(instance.instanceId)}
                    disabled={!instance.principal.enabled || busy}
                    onchange={(event) => {
                      if (event.currentTarget.checked)
                        selectedIds.add(instance.instanceId);
                      else selectedIds.delete(instance.instanceId);
                    }}
                  />
                </td>
                <td>
                  <div class="flex items-center gap-3">
                    <span
                      class="grid h-9 w-9 place-items-center rounded-full bg-base-200 text-base-content/70"
                      ><Icon name="laptop" size={18} /></span
                    >
                    <span class="font-medium">{deviceName(instance)}</span>
                  </div>
                </td>
                <td class="text-base-content/75"
                  >{typeNames.get(instance.deploymentId) ?? "—"}</td
                >
                <td>
                  <span
                    class={[
                      "badge badge-sm",
                      instance.principal.enabled
                        ? "badge-success"
                        : "badge-neutral",
                    ]}>{instance.principal.enabled ? "Active" : "Off"}</span
                  >
                </td>
                <td class="text-base-content/75"
                  >{formatDate(instance.principal.createdAt)}</td
                >
                <td class="text-right">
                  {#if instance.principal.enabled}
                    <ActionMenu
                      widthClass="w-40"
                      ariaLabel={`Actions for ${deviceName(instance)}`}
                    >
                      <li>
                        <a
                          class="text-error"
                          href={resolve(
                            `/admin/devices/instances/disable?instance=${instance.instanceId}`,
                          )}>Turn off</a
                        >
                      </li>
                    </ActionMenu>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
        <div
          class="border-t border-base-300 px-4 py-3 text-sm text-base-content/60"
        >
          Showing {selectedInstances.length} device{selectedInstances.length ===
          1
            ? ""
            : "s"}
        </div>
      {/if}
    </div>
  {/if}
</section>
<ConfirmationModal bind:this={confirmationModal} />
