<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("device-deployment-disable");
  type DisableInput = { deploymentId: string; expectedVersion: Deployment["version"]; idempotencyKey: string; reason: null };
  const mutation = new MutationController<DisableInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let deployments = $state<Deployment[]>([]);
  const requestedDeploymentId = $derived(
    page.url.searchParams.get("deployment") ?? "",
  );
  let selectedDeploymentId = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  /** Set after a successful disable so the old target leaves the active list. */
  let completedDeploymentId = $state<string | null>(null);

  const activeDeployments = $derived(
    deployments.filter((deployment) => deployment.state === "active"),
  );
  const selectedDeployment = $derived(
    activeDeployments.find((deployment) => deployment.deploymentId ===
      (requestedDeploymentId || selectedDeploymentId)) ?? null,
  );
  const requestedUnavailable = $derived(
    requestedDeploymentId !== "" && selectedDeployment === null && !loading && !error && completedDeploymentId !== requestedDeploymentId,

  );

  async function load(requestedId: string) {
    const token = scope.begin();
    const previousSelection = selectedDeploymentId;
    loading = true;
    error = null;
    deployments = [];
    selectedDeploymentId = "";
    try {
      if (requestedId) {
        const response = await trellis.deploymentsGet({ deploymentId: requestedId }).take();
        if (!scope.isCurrent(token)) return;
        if (isErr(response)) {
          if (projectConsoleError(response).code !== "not_found") error = projectConsoleError(response).message;
          return;
        }
        deployments = response.deployment.kind === "device" ? [response.deployment] : [];
      } else {
        const result = await traverseAll<Deployment>(async (request) => {
          const response = await trellis.deploymentsList({ kind: "device", state: "active", page: request }).take();
          if (isErr(response)) throw response;
          return { items: response.items, cursor: response.page.nextCursor ?? undefined };
        });
        if (!scope.isCurrent(token)) return;
        if (!result.complete) { error = projectConsoleError(result.error).message; return; }
        deployments = [...result.items];
        if (deployments.some((deployment) => deployment.deploymentId === previousSelection && deployment.state === "active")) selectedDeploymentId = previousSelection;
      }
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestDisableDeployment() {
    const target = selectedDeployment;
    if (!target || pending || loading || error) return;
    const requestedAtCapture = requestedDeploymentId;
    const key = ulid();
    const intent = captureIntent<DisableInput>({
      operation: "deploymentsDisable", targetId: target.deploymentId, label: target.deploymentId,
      expectedValue: target.deploymentId,
      idempotencyKey: key,
      input: { deploymentId: target.deploymentId, expectedVersion: target.version, idempotencyKey: key, reason: null },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Disable device deployment?",
      message: "This prevents new device activations for the deployment until it is re-enabled.",
      confirmLabel: "Disable deployment",
      targetLabel: "Device deployment",
      targetName: intent.targetId,
      expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && requestedDeploymentId === requestedAtCapture && selectedDeployment?.deploymentId === intent.targetId,

      dispatch: async ({ input }) => await trellis.deploymentsDisable(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedDeploymentId !== requestedAtCapture || false) return;

    if (outcome.kind === "succeeded") {
      completedDeploymentId = intent.targetId;
      notifications.success(`Device deployment ${intent.targetId} disabled.`, "Disabled");
      await load(requestedDeploymentId);
    } else if (outcome.kind === "unknown") {
      error = "Outcome unknown; the disable may have completed. Check the deployment before trying again.";
    } else error = projectConsoleError(outcome.error).message;
  }

  $effect(() => {
    const id = requestedDeploymentId;
    scope.setKey(`device-deployment-disable:${id}`);
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(id));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Disable device deployment" description="Select an active deployment and confirm the disable workflow.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if requestedUnavailable}
    <Notice variant="warning">
      Device deployment '{requestedDeploymentId}' is not an active deployment that can be disabled.
    </Notice>
  {/if}

  {#if completedDeploymentId}
    <Notice variant="success">
      Device deployment {completedDeploymentId} is disabled.
    </Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading device deployments" /></Panel>
  {:else if error}
    <Panel><p class="text-sm">Deployment lookup is incomplete. <button type="button" class="btn btn-ghost btn-xs" onclick={() => void load(requestedDeploymentId)}>Retry</button></p></Panel>
  {:else if requestedUnavailable}
    <EmptyState
      title="Deployment unavailable"
      description={`No active device deployment matches '${requestedDeploymentId}'. It may already be disabled, removed, or the link may be stale.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
      {/snippet}
    </EmptyState>
  {:else if activeDeployments.length === 0}
    <EmptyState title="No active deployments" description="There are no active device deployments available to disable." />
  {:else}
    <Panel title="Confirm deployment disable" eyebrow="Destructive workflow">
      <form class="space-y-4" onsubmit={(event) => { event.preventDefault(); void requestDisableDeployment(); }}>
        <label class="form-control gap-1">
          <span class="label-text text-xs">Deployment</span>
          {#if requestedDeploymentId !== ""}
            <input
              class="input input-bordered input-sm font-mono"
              value={requestedDeploymentId}
              readonly
              aria-label="Deployment"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedDeploymentId} required>
              <option value="" disabled>Select a deployment…</option>
              {#each activeDeployments as deployment (deployment.deploymentId)}
                <option value={deployment.deploymentId}>{deployment.displayName} ({deployment.deploymentId})</option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedDeployment}
          {@const target = selectedDeployment}
          {#if target}
            <div class="rounded-box border border-base-300 bg-base-200/40 p-3 text-sm">
              <div class="trellis-identifier font-medium">{target.deploymentId}</div>
              <div class="text-base-content/60">State: {target.state}</div>
              <div class="text-base-content/60">Delegation: {target.requiresDeviceDelegation ? "required" : "not required"}</div>
              <div class="text-base-content/60">Version: {target.version}</div>
            </div>
          {/if}
        {/if}

        <div class="flex justify-end">
          <button
            type="submit"
            class="btn btn-error btn-sm"
            disabled={pending || !selectedDeployment || requestedUnavailable}
          >
            {pending ? "Disabling…" : "Disable deployment"}
          </button>
        </div>
      </form>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
