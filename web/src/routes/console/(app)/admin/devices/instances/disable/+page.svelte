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

  type Instance = apis.auth.DevicesListOutput["items"][number];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("device-instance-disable");
  type DisableInput = { expectedVersion: Instance["version"]; idempotencyKey: string; instanceId: string; reason: null };
  const mutation = new MutationController<DisableInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let instances = $state<Instance[]>([]);
  const requestedInstanceId = $derived(page.url.searchParams.get("instance") ?? "");
  let selectedInstanceId = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  /** Set after a successful disable so the old target leaves the eligible list. */
  let completedInstanceId = $state<string | null>(null);

  // Only active or pending identities are disable candidates; disabled,
  // revoked, or unknown states are excluded rather than being made eligible.
  const disableableInstances = $derived(
    instances.filter((instance) =>
      instance.state === "active" || instance.state === "pending"
    ),
  );
  const selectedInstance = $derived(
    disableableInstances.find((instance) =>
      instance.instanceId === (requestedInstanceId || selectedInstanceId)
    ) ?? null,
  );
  const requestedUnavailable = $derived(
    requestedInstanceId !== "" && selectedInstance === null && !loading && !error && completedInstanceId !== requestedInstanceId,

  );

  async function load(requestedId: string) {
    const token = scope.begin();
    const previousSelection = selectedInstanceId;
    loading = true;
    error = null;
    instances = [];
    selectedInstanceId = "";
    try {
      const readPage = async (request: { limit: number; cursor?: string }) => {
        const response = await trellis.devicesList({ page: request }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor ?? undefined };
      };
      const result = requestedId
        ? await resolveExact<Instance>(readPage, (instance) => instance.instanceId === requestedId)
        : await traverseAll<Instance>(readPage);
      if (!scope.isCurrent(token)) return;
      if (!result.complete) { error = projectConsoleError(result.error).message; return; }
      instances = requestedId ? ("item" in result && result.item ? [result.item] : []) : ("items" in result ? [...result.items] : []);
      if (!requestedId && instances.some((instance) => instance.instanceId === previousSelection && (instance.state === "active" || instance.state === "pending"))) selectedInstanceId = previousSelection;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestDisableInstance() {
    const target = selectedInstance;
    if (!target || pending || loading || error) return;
    const requestedAtCapture = requestedInstanceId;
    const key = ulid();
    const intent = captureIntent<DisableInput>({
      operation: "devicesDisable", targetId: target.instanceId, label: target.instanceId,
      expectedValue: target.instanceId, idempotencyKey: key,
      input: { expectedVersion: target.version, idempotencyKey: key, instanceId: target.instanceId, reason: null },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Disable device instance?",
      message: "This prevents the device instance from authenticating until it is explicitly re-enabled.",
      confirmLabel: "Disable instance",
      targetLabel: "Device instance",
      targetName: intent.targetId,
      expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && requestedInstanceId === requestedAtCapture && selectedInstance?.instanceId === intent.targetId,

      dispatch: async ({ input }) => await trellis.devicesDisable(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedInstanceId !== requestedAtCapture || false) return;

    if (outcome.kind === "succeeded") {
      completedInstanceId = intent.targetId;
      notifications.success(`Device instance ${intent.targetId} disabled.`, "Disabled");
      await load(requestedInstanceId);
    } else if (outcome.kind === "unknown") {
      error = "Outcome unknown; the disable may have completed. Check the instance before trying again.";
    } else error = projectConsoleError(outcome.error).message;
  }

  $effect(() => {
    const id = requestedInstanceId;
    scope.setKey(`device-instance-disable:${id}`);
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(id));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Disable device instance" description="Select a device instance and confirm the disable workflow.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if requestedUnavailable}
    <Notice variant="warning">
      Device instance '{requestedInstanceId}' is not an active instance that can be disabled.
    </Notice>
  {/if}

  {#if completedInstanceId}
    <Notice variant="success">
      Device instance {completedInstanceId} is disabled.
    </Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading device instances" /></Panel>
  {:else if error}
    <Panel><p class="text-sm">Instance lookup is incomplete. <button type="button" class="btn btn-ghost btn-xs" onclick={() => void load(requestedInstanceId)}>Retry</button></p></Panel>
  {:else if requestedUnavailable}
    <EmptyState
      title="Instance unavailable"
      description={`No active device instance matches '${requestedInstanceId}'. It may already be disabled, removed, or the link may be stale.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
      {/snippet}
    </EmptyState>
  {:else if disableableInstances.length === 0}
    <EmptyState title="No instances available" description="There are no non-disabled device instances available to disable." />
  {:else}
    <Panel title="Confirm instance disable" eyebrow="Destructive workflow">
      <form class="space-y-4" onsubmit={(event) => { event.preventDefault(); void requestDisableInstance(); }}>
        <label class="form-control gap-1">
          <span class="label-text text-xs">Instance</span>
          {#if requestedInstanceId !== ""}
            <input
              class="input input-bordered input-sm font-mono"
              value={requestedInstanceId}
              readonly
              aria-label="Instance"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedInstanceId} required>
              <option value="" disabled>Select an instance…</option>
              {#each disableableInstances as instance (`${instance.instanceId}:${instance.createdAt}`)}
                <option value={instance.instanceId}>{instance.instanceId} · {instance.deploymentId} · {instance.state}</option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedInstance}
          {@const target = selectedInstance}
          {#if target}
            <div class="rounded-box border border-base-300 bg-base-200/40 p-3 text-sm">
              <div class="trellis-identifier font-medium">{target.instanceId}</div>
              <div class="text-base-content/60">Deployment: {target.deploymentId}</div>
              <div class="text-base-content/60">State: {target.state}</div>
              <div class="text-base-content/60">Created: {formatDate(target.createdAt)}</div>
            </div>
          {/if}
        {/if}

        <div class="flex justify-end">
          <button type="submit" class="btn btn-error btn-sm" disabled={pending || !selectedInstance || requestedUnavailable}>
            {pending ? "Disabling…" : "Disable instance"}
          </button>
        </div>
      </form>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
