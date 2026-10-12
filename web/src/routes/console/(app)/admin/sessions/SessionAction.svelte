<script lang="ts">
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  let { kind }: { kind: "revoke" | "kick" } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("session-action");
  const queryKey = $derived(
    kind === "revoke" ? "authorizationSessionId" : "attachmentId",
  );
  const requestedId = $derived(page.url.searchParams.get(queryKey) ?? "");
  let records = $state.raw<{ id: string; description: string }[]>([]);
  let selectedId = $state("");
  let loading = $state(true);
  let busy = $state(false);
  let completed = $state(false);
  let uncertain = $state(false);
  let error = $state<string | null>(null);
  let enforcement = $state<
    apis.auth.AuthorizationSessionsRevokeOutput["enforcement"] | null
  >(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  const target = $derived(
    records.find((item) => item.id === (requestedId || selectedId)),
  );

  $effect(() => {
    const id = requestedId;
    const action = kind;
    if (scope.setKey(`${action}:${id}`)) {
      completed = false;
      uncertain = false;
      enforcement = null;
      selectedId = "";
    }
    void load(action);
  });

  async function load(action: "revoke" | "kick"): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    if (action === "revoke") {
      const result = await traverseAll(async (page) => {
        const response = await trellis
          .authorizationSessionsList({ page })
          .orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.settle(token)) return;
      records = result.items
        .filter((item) => item.state === "active")
        .map((item) => ({
          id: item.authorizationSessionId,
          description: `${item.principalId} · ${item.binding.kind} · ${item.runtimeId}`,
        }));
      if (!result.complete) error = projectConsoleError(result.error).message;
    } else {
      const result = await traverseAll(async (page) => {
        const response = await trellis.connectionsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.settle(token)) return;
      records = result.items
        .filter((item) => item.state === "admitted")
        .map((item) => ({
          id: item.attachmentId,
          description: `${item.serverId} · broker client ${item.brokerClientId} · session ${item.authorizationSessionId}`,
        }));
      if (!result.complete) error = projectConsoleError(result.error).message;
    }
    loading = false;
  }

  async function execute(): Promise<void> {
    if (
      !target ||
      loading ||
      error ||
      busy ||
      completed ||
      uncertain ||
      !canPerform(
        authority.authority,
        kind === "revoke" ? "authorizationSessionsRevoke" : "connectionsKick",
      )
    )
      return;
    const captured = target;
    const action = kind;
    const capturedKey = scope.key;
    const key = ulid();
    const confirmed = await confirmationModal?.confirm({
      title:
        action === "revoke"
          ? "Revoke authorization session?"
          : "Kick attachment?",
      message:
        action === "revoke"
          ? "Retire this logical session and queue physical enforcement."
          : "Close this physical attachment. The logical session and OAuth grant remain valid.",
      confirmLabel: action === "revoke" ? "Revoke" : "Kick",
      targetLabel: queryKey,
      targetName: captured.id,
      expectedValue: captured.id,
    });
    if (
      !confirmed ||
      scope.disposed ||
      scope.key !== capturedKey ||
      target?.id !== captured.id
    )
      return;
    busy = true;
    try {
      const response =
        action === "revoke"
          ? await trellis
              .authorizationSessionsRevoke(
                apis.auth.API.actions[
                  "rpc:AuthorizationSessions.Revoke"
                ].input.decode({
                  authorizationSessionId: captured.id,
                  idempotencyKey: key,
                }),
              )
              .orThrow()
          : await trellis
              .connectionsKick(
                apis.auth.API.actions["rpc:Connections.Kick"].input.decode({
                  attachmentId: captured.id,
                  idempotencyKey: key,
                }),
              )
              .orThrow();
      if (scope.disposed || scope.key !== capturedKey) return;
      enforcement = response.enforcement;
      completed = true;
    } catch (cause) {
      if (scope.disposed || scope.key !== capturedKey) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Outcome unknown. Inspect this exact target before issuing another action."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar
    title={kind === "revoke"
      ? "Revoke authorization session"
      : "Kick attachment"}
    description={kind === "revoke"
      ? "Logical retirement and physical connection enforcement are separate."
      : "A physical kick does not revoke logical authority."}
    >{#snippet actions()}<a
        class="btn btn-ghost btn-sm"
        href={resolve("/admin/sessions")}>Back to sessions</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if completed && enforcement}<Notice variant="success"
      >Accepted. Enforcement {enforcement.workId}: {enforcement.state}. Scanned {enforcement.scannedSessions}
      sessions; retired {enforcement.retiredSessions}; pending kicks {enforcement.pendingKicks};
      failed kicks {enforcement.failedKicks}.</Notice
    >{/if}
  <Panel title="Exact target" class="max-w-3xl">
    {#if loading}<LoadingState
        label="Loading eligible targets"
      />{:else if requestedId && !target}<Notice variant="warning"
        >The requested target is unavailable or no longer eligible. No other
        target was substituted.</Notice
      >{:else}
      <form
        class="trellis-form"
        onsubmit={(event) => {
          event.preventDefault();
          void execute();
        }}
      >
        <label class="trellis-field"
          ><span class="trellis-field-label">{queryKey}</span
          >{#if requestedId}<input
              class="input input-bordered input-sm font-mono"
              value={requestedId}
              readonly
            />{:else}<select
              class="select select-bordered select-sm"
              bind:value={selectedId}
              ><option value="">Select an exact target</option
              >{#each records as item (item.id)}<option value={item.id}
                  >{item.id} · {item.description}</option
                >{/each}</select
            >{/if}</label
        >
        {#if target}<p class="trellis-field-help">{target.description}</p>{/if}
        <div class="trellis-action-row">
          <button
            class="btn btn-error btn-outline btn-sm"
            disabled={!target || !!error || busy || completed || uncertain}
            type="submit"
            >{kind === "revoke" ? "Revoke session" : "Kick attachment"}</button
          >
        </div>
      </form>
    {/if}
  </Panel>
</section>
<ConfirmationModal bind:this={confirmationModal} />
