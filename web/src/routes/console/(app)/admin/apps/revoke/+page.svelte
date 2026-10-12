<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { page } from "$app/state";
  import { onMount, untrack } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  type Delegation = apis.auth.DelegationsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("revoke-delegation");
  const requestedId = $derived(page.url.searchParams.get("delegationId") ?? "");
  let selectedId = $state("");
  let records = $state.raw<Delegation[]>([]);
  let loading = $state(true);
  let busy = $state(false);
  let completed = $state(false);
  let uncertain = $state(false);
  let error = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  const target = $derived(
    records.find((item) => item.delegationId === (requestedId || selectedId)),
  );
  $effect(() => {
    const id = requestedId;
    if (scope.setKey(id)) {
      completed = false;
      uncertain = false;
      selectedId = "";
    }
    untrack(() => {
      confirmationModal?.cancel();
      void load(id);
    });
  });
  async function load(id: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      if (id) {
        const input = apis.auth.API.actions[
          "rpc:Delegations.Review"
        ].input.decode({ delegationId: id });
        const result = await trellis.delegationsReview(input).orThrow();
        if (scope.isCurrent(token)) records = [result.delegation];
      } else {
        const result = await traverseAll(async (page) => {
          const response = await trellis.delegationsList({ page }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        });
        if (!scope.isCurrent(token)) return;
        records = [...result.items];
        if (!result.complete) error = projectConsoleError(result.error).message;
      }
    } catch (cause) {
      if (scope.isCurrent(token)) {
        records = [];
        error = projectConsoleError(cause).message;
      }
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }
  async function revoke(): Promise<void> {
    if (
      !target ||
      loading ||
      busy ||
      error ||
      completed ||
      uncertain ||
      !canPerform(authority.authority, "delegationsRevoke")
    )
      return;
    const captured = target;
    const token = scope.begin();
    const key = ulid();
    const confirmed = await confirmationModal?.confirm({
      title: "Revoke app delegation?",
      message:
        "Remove the exact client's delegated consent authority for this principal.",
      confirmLabel: "Revoke delegation",
      targetLabel: "Delegation",
      targetName: captured.delegationId,
      expectedValue: captured.delegationId,
      details: `Principal ${captured.principalId} · Client ${captured.binding.clientId} · Revision ${captured.revision}`,
    });
    if (
      !confirmed ||
      !scope.isCurrent(token) ||
      target?.revision !== captured.revision ||
      target.delegationId !== captured.delegationId
    )
      return;
    busy = true;
    try {
      await trellis
        .delegationsRevoke({
          delegationId: captured.delegationId,
          expectedRevision: captured.revision,
          idempotencyKey: key,
        })
        .orThrow();
      if (scope.isCurrent(token)) completed = true;
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Outcome unknown. Inspect this exact delegation before another action."
        : projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) busy = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar
    title="Revoke app delegation"
    description="Confirm the exact client-bound delegation and its current revision."
    >{#snippet actions()}<a
        class="btn btn-ghost btn-sm"
        href={resolve("/admin/apps")}>Back to app delegations</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if completed}<Notice variant="success">Delegation revoked.</Notice>{/if}
  <Panel title="Delegation target" class="max-w-3xl">
    {#if loading}<LoadingState
        label="Loading exact delegation"
      />{:else if requestedId && !target}<Notice variant="warning"
        >The exact delegation is unavailable. No other target was substituted.</Notice
      >{:else}
      <form
        class="trellis-form"
        onsubmit={(event) => {
          event.preventDefault();
          void revoke();
        }}
      >
        <label class="trellis-field"
          ><span class="trellis-field-label">Delegation</span
          >{#if requestedId}<input
              class="input input-bordered input-sm font-mono"
              value={requestedId}
              readonly
            />{:else}<select
              class="select select-bordered select-sm"
              bind:value={selectedId}
              ><option value="">Select an exact delegation</option
              >{#each records as item (item.delegationId)}<option
                  value={item.delegationId}
                  >{item.delegationId} · {item.binding.clientId} · {item.principalId}</option
                >{/each}</select
            >{/if}</label
        >{#if target}<p class="trellis-field-help">
            Principal {target.principalId} · {target.binding.kind} client {target
              .binding.clientId} · {target.binding.origin ?? "Native client"} · Revision
            {target.revision}
          </p>{/if}
        <div class="trellis-action-row">
          <button
            class="btn btn-error btn-outline btn-sm"
            type="submit"
            disabled={!target ||
              loading ||
              busy ||
              !!error ||
              completed ||
              uncertain}>Revoke delegation</button
          >
        </div>
      </form>
    {/if}
  </Panel>
</section>
<ConfirmationModal bind:this={confirmationModal} />
