<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oats-center/result";
  import { type apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { traverseAll, resolveExact } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type Activation = apis.auth.DeviceUserAuthoritiesListOutput["items"][number];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("device-activation-revoke");
  type RevokeInput = { deploymentId: string; devicePrincipalId: string; idempotencyKey: string; reason: null };
  const mutation = new MutationController<RevokeInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let activations = $state<Activation[]>([]);
  const requestedInstanceId = $derived(
    page.url.searchParams.get("instance") ?? "",
  );
  let selectedInstanceId = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  /** Set after a successful revoke so the old target leaves the active list. */
  let completedInstanceId = $state<string | null>(null);

  const activeActivations = $derived(
    activations.filter((activation) =>
      activation.device.delegationState === "active"
    ),
  );
  const selectedActivation = $derived(
    activeActivations.find((activation) =>
      activation.device.instanceId === (requestedInstanceId || selectedInstanceId)
    ) ?? null,
  );
  const requestedUnavailable = $derived(
    requestedInstanceId !== "" && selectedActivation === null && !loading && !error && completedInstanceId !== requestedInstanceId,

  );

  async function load(requestedId: string) {
    const token = scope.begin();
    const previousSelection = selectedInstanceId;
    loading = true;
    error = null;
    activations = [];
    selectedInstanceId = "";
    try {
      const readPage = async (request: { limit: number; cursor?: string }) => {
        const response = await trellis.deviceUserAuthoritiesList({ page: request }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor ?? undefined };
      };
      const result = requestedId
        ? await resolveExact<Activation>(readPage, (activation) => activation.device.instanceId === requestedId)
        : await traverseAll<Activation>(readPage);
      if (!scope.isCurrent(token)) return;
      if (!result.complete) { error = projectConsoleError(result.error).message; return; }
      activations = requestedId ? ("item" in result && result.item ? [result.item] : []) : ("items" in result ? [...result.items] : []);
      if (!requestedId && activations.some((activation) => activation.device.instanceId === previousSelection && activation.device.delegationState === "active")) selectedInstanceId = previousSelection;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestRevokeActivation() {
    const target = selectedActivation;
    if (!target || pending || loading || error) return;
    const requestedAtCapture = requestedInstanceId;
    const key = ulid();
    const intent = captureIntent<RevokeInput>({
      operation: "deviceUserAuthoritiesRevoke", targetId: target.device.instanceId, label: target.device.instanceId,
      expectedValue: target.device.instanceId, idempotencyKey: key,
      input: { deploymentId: target.device.deploymentId, devicePrincipalId: target.device.principalId, idempotencyKey: key, reason: null },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Revoke device activation?",
      message: "This terminates the active user delegation for this device instance.",
      confirmLabel: "Revoke activation",
      targetLabel: "Device instance",
      targetName: intent.targetId,
      expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && requestedInstanceId === requestedAtCapture && selectedActivation?.device.instanceId === intent.targetId,

      dispatch: async ({ input }) => await trellis.deviceUserAuthoritiesRevoke(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedInstanceId !== requestedAtCapture || false) return;

    if (outcome.kind === "succeeded") {
      completedInstanceId = intent.targetId;
      notifications.success(`Device activation revoked for ${intent.targetId}.`, "Revoked");
      await load(requestedInstanceId);
    } else if (outcome.kind === "unknown") {
      error = "Outcome unknown; the revoke may have completed. Check the activation before trying again.";
    } else error = projectConsoleError(outcome.error).message;
  }

  $effect(() => {
    const id = requestedInstanceId;
    scope.setKey(`device-activation-revoke:${id}`);
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(id));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Revoke device activation" description="Select an activated device instance and confirm revocation.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if requestedUnavailable}
    <Notice variant="warning">
      Device instance '{requestedInstanceId}' has no active activation to revoke.
    </Notice>
  {/if}

  {#if completedInstanceId}
    <Notice variant="success">
      Device activation revoked for {completedInstanceId}.
    </Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading active device activations" /></Panel>
  {:else if error}
    <Panel><p class="text-sm">Activation lookup is incomplete. <button type="button" class="btn btn-ghost btn-xs" onclick={() => void load(requestedInstanceId)}>Retry</button></p></Panel>
  {:else if requestedUnavailable}
    <EmptyState
      title="Activation unavailable"
      description={`No active activation matches '${requestedInstanceId}'. It may already be revoked, or the link may be stale.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
      {/snippet}
    </EmptyState>
  {:else if activeActivations.length === 0}
    <EmptyState title="No active activations" description="There are no activated device instances available to revoke." />
  {:else}
    <Panel title="Confirm activation revoke" eyebrow="Destructive workflow">
      <form class="space-y-4" onsubmit={(event) => { event.preventDefault(); void requestRevokeActivation(); }}>
        <label class="form-control gap-1">
          <span class="label-text text-xs">Activated instance</span>
          {#if requestedInstanceId !== ""}
            <input
              class="input input-bordered input-sm font-mono"
              value={requestedInstanceId}
              readonly
              aria-label="Activated instance"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedInstanceId} required>
              <option value="" disabled>Select an activated instance…</option>
              {#each activeActivations as activation (activation.device.instanceId)}
                <option value={activation.device.instanceId}>{activation.device.instanceId} · {activation.device.deploymentId}</option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedActivation}
          {@const target = selectedActivation}
          {#if target}
            <div class="rounded-box border border-base-300 bg-base-200/40 p-3 text-sm">
              <div class="trellis-identifier font-medium">{target.device.instanceId}</div>
              <div class="text-base-content/60">Deployment: {target.device.deploymentId}</div>
              <div class="text-base-content/60">Created: {formatDate(target.device.createdAt)}</div>
              <div class="text-base-content/60">Principal: <span class="trellis-identifier">{target.device.principalId}</span></div>
            </div>
          {/if}
        {/if}

        <div class="flex justify-end">
          <button
            type="submit"
            class="btn btn-error btn-sm"
            disabled={pending || !selectedActivation || requestedUnavailable}
          >
            {pending ? "Revoking…" : "Revoke activation"}
          </button>
        </div>
      </form>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
