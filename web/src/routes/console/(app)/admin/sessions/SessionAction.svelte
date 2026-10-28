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
  import { formatDate, humanizeName } from "$lib/format";

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
    // Names are best-effort context; the ID stays the exact target.
    const users = await traverseAll(async (page) => {
      const response = await trellis.usersList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    const names = new Map(
      users.items.map((user) => [user.principalId, user.displayName]),
    );
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
          description: `${names.get(item.principalId) ?? humanizeName(item.binding.participantId ?? "Service")} · ${humanizeName(item.binding.clientId ?? item.binding.participantId ?? "")} · signed in ${formatDate(item.createdAt)}`,
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
          description: item.admittedAt
            ? `Connected ${formatDate(item.admittedAt)}`
            : "Connecting",
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
      title: action === "revoke" ? "Sign out this session?" : "Disconnect?",
      message:
        action === "revoke"
          ? "They'll need to sign in again."
          : "The connection closes. They stay signed in and may reconnect.",
      confirmLabel: action === "revoke" ? "Sign out" : "Disconnect",
      targetLabel: action === "revoke" ? "Session" : "Connection",
      targetName: captured.description,
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
        ? "We couldn't confirm the change. Check the session before trying again."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="max-w-3xl space-y-5">
  <PageToolbar
    eyebrow="Sessions"
    title={kind === "revoke" ? "Sign out a session" : "Disconnect"}
    description={kind === "revoke"
      ? "They'll need to sign in again."
      : "The connection closes. They stay signed in and may reconnect."}
    >{#snippet actions()}<a
        class="btn btn-ghost"
        href={resolve("/admin/sessions")}>Back to sessions</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if completed && enforcement}<Notice variant="success"
      >{kind === "revoke" ? "Signed out." : "Disconnected."}
      {enforcement.pendingKicks || enforcement.failedKicks
        ? "Some connections are still closing."
        : ""}</Notice
    >{/if}
  <Panel>
    {#if loading}<LoadingState
        label="Loading sessions"
      />{:else if requestedId && !target}<Notice variant="warning"
        >This session has already ended or doesn't exist.</Notice
      >{:else}
      <form
        class="trellis-form"
        onsubmit={(event) => {
          event.preventDefault();
          void execute();
        }}
      >
        {#if target}<p class="font-medium">{target.description}</p>{/if}
        <div class="trellis-field">
          <label class="trellis-field-label" for="session-target"
            >{kind === "revoke" ? "Session" : "Connection"}</label
          >{#if requestedId}<input
              id="session-target"
              class="input input-sm w-full font-mono text-xs text-base-content/70"
              value={requestedId}
              readonly
            />{:else}<select
              id="session-target"
              class="select w-full"
              bind:value={selectedId}
              ><option value=""
                >Choose a {kind === "revoke"
                  ? "session"
                  : "connection"}…</option
              >{#each records as item (item.id)}<option value={item.id}
                  >{item.description}</option
                >{/each}</select
            >{/if}
        </div>
        <div class="trellis-action-row">
          <button
            class="btn btn-error"
            disabled={!target || !!error || busy || completed || uncertain}
            type="submit"
            >{kind === "revoke" ? "Sign out" : "Disconnect"}</button
          >
        </div>
      </form>
    {/if}
  </Panel>
</section>
<ConfirmationModal bind:this={confirmationModal} />
