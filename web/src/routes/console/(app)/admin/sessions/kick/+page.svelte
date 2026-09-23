<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oats-center/result";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { catalogPage, resolveExact, traverseAll } from "$lib/console/paging.ts";
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
  import {
    describeSessionPrincipal,
    formatShortKey,
    type ConnectionRecord,
  } from "$lib/auth_display.ts";
  import { formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("connection-kick");
  type KickInput = { connectionId: string; idempotencyKey: string; reason: null };
  let pending = $state(false);
  const mutation = new MutationController<KickInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let connections = $state.raw<ConnectionRecord[]>([]);
  let selectedConnectionId = $state("");
  /** Set after a successful kick so the departed row is not auto-replaced. */
  let completedConnectionId = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();

  // Canonical query name; the legacy `userNkey` name is not accepted.
  const requestedConnectionId = $derived(
    page.url.searchParams.get("connectionId") ?? "",
  );
  const selectedConnection = $derived(
    connections.find((connection) =>
      connection.connectionId === (requestedConnectionId || selectedConnectionId)
    ) ?? null,
  );
  const selectedConnectionIsCurrent = $derived(
    selectedConnection?.connectionId === authority.identity.connectionId,
  );
  const requestedUnavailable = $derived(
    requestedConnectionId !== "" && selectedConnection === null && completedConnectionId !== requestedConnectionId && !error,

  );

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  async function load(requestedId: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    connections = [];
    selectedConnectionId = "";
    try {
      if (requestedId !== "") {
        const resolved = await resolveExact<ConnectionRecord>(
          async (pageRequest) => {
            const response = await trellis.connectionsList({
              page: catalogPage(pageRequest.cursor),
            }).take();
            if (isErr(response)) throw response;
            return { items: response.items, cursor: response.page.nextCursor ?? undefined };
          },
          (connection) => connection.connectionId === requestedId,
        );
        if (!scope.isCurrent(token)) return;
        if (!resolved.complete) {
          error = failureFrom(resolved.error);
          return;
        }
        connections = resolved.item === undefined ? [] : [resolved.item];
      } else {
        const catalog = await traverseAll<ConnectionRecord>(async (pageRequest) => {
          const response = await trellis.connectionsList({ page: pageRequest }).take();
          if (isErr(response)) throw response;
          return { items: response.items, cursor: response.page.nextCursor ?? undefined };
        });
        if (!scope.isCurrent(token)) return;
        if (!catalog.complete) {
          error = failureFrom(catalog.error);
          return;
        }
        connections = [...catalog.items];
      }
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestKickConnection(): Promise<void> {
    const target = selectedConnection;
    if (!target || pending) return;
    const summary = describeSessionPrincipal(target);
    const requestedAtCapture = requestedConnectionId;
    const isCurrent = target.connectionId === authority.identity.connectionId;
    const key = ulid();
    const intent = captureIntent({
      operation: "connectionsKick", targetId: target.connectionId, label: summary.title,
      expectedValue: target.connectionId, idempotencyKey: key,
      input: { connectionId: target.connectionId, idempotencyKey: key, reason: null },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: isCurrent
        ? "Disconnect this console's connection?"
        : "Kick connection?",
      message: isCurrent
        ? "This disconnects the console's own transport. The session stays valid and the console may reconnect automatically."
        : "This immediately disconnects the selected active connection. Its session remains valid.",
      confirmLabel: "Kick connection",
      targetLabel: intent.label, targetName: intent.targetId, expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && selectedConnection?.connectionId === intent.targetId,

      dispatch: async ({ input }) => await trellis.connectionsKick(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedConnectionId !== requestedAtCapture) return;
    if (outcome.kind === "succeeded") {
      completedConnectionId = intent.targetId;
      if (selectedConnectionId === intent.targetId) selectedConnectionId = "";
      if (true) {
        notifications.success(`Disconnected ${intent.label}. The session stays valid and may reconnect.`, "Kicked");
        await load(requestedAtCapture);
      }
    } else if (outcome.kind === "unknown") {
      error = { message: "Outcome unknown; the kick request may have completed. Check the connections list before starting a new operation." };
    } else {
      error = failureFrom(outcome.error);
    }
  }

  $effect(() => {
    const requestedId = requestedConnectionId;
    scope.setKey(`connection-kick:${requestedId}`);
    completedConnectionId = null;
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(requestedId));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Kick connection" description="Confirm and disconnect one exact active connection.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/sessions")}>Back to sessions</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
       <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={() => void load(requestedConnectionId)}>Retry</button>
    </Notice>
  {/if}

  {#if loading && !error}
    <Panel><LoadingState label="Loading connections" /></Panel>
  {:else if error}
    <Panel><p class="text-sm text-base-content/60">Connection lookup is incomplete. Retry before selecting a target.</p></Panel>
  {:else if requestedUnavailable || connections.length === 0}
    <EmptyState
      title="Connection unavailable"
      description={requestedConnectionId !== ""
        ? `No connection matches '${requestedConnectionId}'. It may already be closed, or the link may be stale.`
        : "No active connections are available to kick."}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/sessions")}>Back to sessions</a>
      {/snippet}
    </EmptyState>
  {:else}
    <Panel title="Confirm kick" eyebrow="Workflow">
      <div class="space-y-4">
        <label class="form-control gap-1">
          <span class="label-text text-xs">Connection</span>
          {#if requestedConnectionId !== ""}
            <input
              class="input input-bordered input-sm font-mono"
              value={requestedConnectionId}
              readonly
              aria-label="Connection"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedConnectionId} required>
              <option value="" disabled>Select a connection…</option>
              {#each connections as connection (connection.connectionId)}
                <option value={connection.connectionId}>
                  {describeSessionPrincipal(connection).title} · {connection.connectionId}
                </option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedConnection}
          <div class="rounded-box border border-base-300 p-3 text-sm">
            <div class="font-medium">{describeSessionPrincipal(selectedConnection).title}</div>
            <div class="trellis-identifier text-base-content/60">{selectedConnection.connectionId}</div>
            <div class="text-base-content/60">Key {formatShortKey(selectedConnection.userNkey)}</div>
            <div class="text-base-content/60">Connected {formatDate(selectedConnection.connectedAt)}</div>
            {#if selectedConnection.loginSessionId}
              <div class="trellis-identifier text-base-content/60">Session {selectedConnection.loginSessionId}</div>
            {/if}
            {#if selectedConnectionIsCurrent}
              <div class="mt-2"><span class="badge badge-warning badge-sm">This console's connection</span></div>
            {/if}
          </div>
        {/if}

        <p class="trellis-field-help">
          A kick disconnects the transport connection. It does not revoke the
          underlying session, and the client may reconnect.
        </p>

        <div class="flex flex-wrap gap-2">
          <button
            class="btn btn-error btn-sm"
            onclick={requestKickConnection}
             disabled={!selectedConnection || pending || !!error}
          >
            {pending ? "Kicking..." : "Kick connection"}
          </button>
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/sessions")}>Cancel</a>
        </div>
      </div>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
