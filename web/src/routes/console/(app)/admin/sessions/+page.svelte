<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import { consoleUrl } from "$lib/console_paths";
  import { TABLE_PAGE_LIMIT, tablePage } from "$lib/console/paging.ts";
  import { captureIntent, classifyMutationError, isIntentCurrent, type MutationIntent } from "$lib/console/mutation.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { SvelteSet } from "svelte/reactivity";
  import {
    describeSessionPrincipal,
    formatShortKey,
    participantKindBadgeClass,
    participantKindLabel,
    type ConnectionRecord,
    type SessionRecord,
  } from "$lib/auth_display.ts";
  import { errorMessage, formatDate } from "$lib/format";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import Term from "$lib/components/Term.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { getTrellis } from "$lib/trellis";
  import { bulkExpectedCount, bulkTargetDetails, runBulk, toggleAll, toggleId } from "$lib/bulk.ts";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();

  let activeTab = $state<"sessions" | "connections">(page.url.searchParams.get("tab") === "connections" ? "connections" : "sessions");

  // Sessions and connections are independent views with their own loading,
  // error, page state, and selection.
  let sessions = $state.raw<SessionRecord[]>([]);
  let sessionsLoading = $state(true);
  let sessionsError = $state<string | null>(null);
  let sessionFilterUser = $state("");
  let sessionCursor = $state<string | undefined>(undefined);

  let connections = $state.raw<ConnectionRecord[]>([]);
  let connectionsLoading = $state(true);
  let connectionsError = $state<string | null>(null);
  let connFilterSessionId = $state("");
  let connCursor = $state<string | undefined>(undefined);

  const selectedSessions = new SvelteSet<string>();
  const selectedConnections = new SvelteSet<string>();
  let bulkBusy = $state(false);
  let sessionResult = $state<{ succeeded: number; failed: string[] } | null>(null);
  let connectionResult = $state<{ succeeded: number; failed: string[] } | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  let uncertain = $state(false);
  let unknownLabels = $state<string[]>([]);
  let disposed = false;

  type SessionsRevokeIntent = MutationIntent<apis.auth.SessionsRevokeInput>;
  type ConnectionsKickIntent = MutationIntent<apis.auth.ConnectionsKickInput>;

  const loading = $derived(
    activeTab === "sessions" ? sessionsLoading : connectionsLoading,
  );
  const error = $derived(
    activeTab === "sessions" ? sessionsError : connectionsError,
  );
  // A session may exist without a current connection; the current session is
  // identified by ID, never by row order or a legacy key field.
  const revocableSessions = $derived(
    sessions.filter((session) => !isCurrentSession(session)),
  );
  const selectableSessionIds = $derived(
    revocableSessions.map((session) => session.sessionId),
  );
  // A connection kick does not revoke the session and the client may reconnect,
  // so the current connection is excluded from generic bulk kick.
  const kickableConnections = $derived(
    connections.filter((connection) => connection.connectionId !== authority.identity.connectionId),
  );
  const selectableConnectionIds = $derived(
    kickableConnections.map((connection) => connection.connectionId),
  );

  async function loadSessions() {
    sessionsLoading = true;
    sessionsError = null;
    try {
      const response = await trellis.sessionsList({
        ...(sessionFilterUser.trim() === ""
          ? {}
          : { principalId: sessionFilterUser.trim() }),
        page: tablePage(sessionCursor),
      }).take();
      if (isErr(response)) {
        sessionsError = errorMessage(response);
        return;
      }
      sessions = response.items ?? [];
      sessionCursor = response.page.nextCursor;
      // Selection only ever holds IDs still present in the current page.
      for (const id of [...selectedSessions]) {
        if (!sessions.some((session) => session.sessionId === id)) {
          selectedSessions.delete(id);
        }
      }
    } catch (e) {
      sessionsError = errorMessage(e);
    } finally {
      sessionsLoading = false;
    }
  }

  async function loadConnections() {
    connectionsLoading = true;
    connectionsError = null;
    try {
      const response = await trellis.connectionsList({
        ...(connFilterSessionId.trim() === ""
          ? {}
          : { sessionId: connFilterSessionId.trim() }),
        page: tablePage(connCursor),
      }).take();
      if (isErr(response)) {
        connectionsError = errorMessage(response);
        return;
      }
      connections = response.items ?? [];
      connCursor = response.page.nextCursor;
      for (const id of [...selectedConnections]) {
        if (!connections.some((connection) => connection.connectionId === id)) {
          selectedConnections.delete(id);
        }
      }
    } catch (e) {
      connectionsError = errorMessage(e);
    } finally {
      connectionsLoading = false;
    }
  }

  function applySessionFilter(): void {
    sessionCursor = undefined;
    selectedSessions.clear();
    void loadSessions();
  }

  function applyConnectionFilter(): void {
    connCursor = undefined;
    selectedConnections.clear();
    void loadConnections();
  }

  function loadActive() {
    if (activeTab === "sessions") void loadSessions();
    else void loadConnections();
  }

  function isCurrentSession(session: SessionRecord): boolean {
    return session.sessionId === authority.identity.loginSessionId;
  }

  async function revokeSessions(intents: SessionsRevokeIntent[]) {
    bulkBusy = true;
    sessionResult = null;
    const outcome = await runBulk(intents, async (intent) => {
      if (
        disposed || !isIntentCurrent(intent, {

          routeKey: "/admin/sessions",
        }) || !sessions.some((session) =>

          session.sessionId === intent.targetId && session.version === intent.input.expectedVersion

        )
      ) {
        throw new Error("Target or authority changed before dispatch; no revoke sent.");
      }
      await trellis.sessionsRevoke(intent.input).orThrow();
    }, (cause) => classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed");
    if (disposed) { bulkBusy = false; return; }
    for (const intent of intents) selectedSessions.delete(intent.targetId);
    sessionResult = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((failure) => `${failure.target.label}: ${failure.reason}`),
    };
    if (outcome.unknown.length > 0) {
      uncertain = true;
      unknownLabels = outcome.unknown.map((item) => item.target.label);
    }
    bulkBusy = false;
    void loadSessions();
  }

  async function requestBulkRevoke() {
    if (bulkBusy || uncertain) return;
    const targets = revocableSessions.filter((session) => selectedSessions.has(session.sessionId));
    if (targets.length === 0) return;
    const intents = targets.map((session) => {
      const idempotencyKey = ulid();
      return captureIntent<apis.auth.SessionsRevokeInput>({
        operation: "sessionsRevoke",
        input: {
          expectedVersion: session.version,
          idempotencyKey,
          reason: null,
          sessionId: session.sessionId,
        },
        label: describeSessionPrincipal(session).title,
        targetId: session.sessionId,
        scope: {
          routeKey: "/admin/sessions",
        },
        idempotencyKey,
      });
    });
    const confirmed = await confirmationModal?.confirm({
      title: `Revoke ${targets.length} session${targets.length === 1 ? "" : "s"}?`,
      message: "Each session loses its credentials and active connections immediately.",
      confirmLabel: `Revoke ${targets.length}`,
      targetLabel: "Sessions",
      targetName: `${targets.length} sessions`,
      expectedValue: bulkExpectedCount(targets.length),
      details: bulkTargetDetails(intents.map((intent) => intent.label)),
    });
    if (
      !confirmed || disposed || uncertain || intents.some((intent) =>

        !isIntentCurrent(intent, {
          routeKey: "/admin/sessions",
        })
      )
    ) return;
    await revokeSessions(intents);
  }

  async function kickConnections(intents: ConnectionsKickIntent[]) {
    bulkBusy = true;
    connectionResult = null;
    const outcome = await runBulk(intents, async (intent) => {
      if (
        disposed || !isIntentCurrent(intent, {

          routeKey: "/admin/sessions",
        }) || intent.targetId === authority.identity.connectionId ||

        !connections.some((connection) => connection.connectionId === intent.targetId)
      ) {
        throw new Error("Target or authority changed before dispatch; no disconnect sent.");
      }
      await trellis.connectionsKick(intent.input).orThrow();
    }, (cause) => classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed");
    if (disposed) { bulkBusy = false; return; }
    for (const intent of intents) selectedConnections.delete(intent.targetId);
    connectionResult = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((failure) => `${failure.target.label}: ${failure.reason}`),
    };
    if (outcome.unknown.length > 0) {
      uncertain = true;
      unknownLabels = outcome.unknown.map((item) => item.target.label);
    }
    bulkBusy = false;
    void loadConnections();
  }

  async function requestBulkKick() {
    if (bulkBusy || uncertain) return;
    const targets = kickableConnections.filter((connection) => selectedConnections.has(connection.connectionId));
    if (targets.length === 0) return;
    const intents = targets.map((connection) => {
      const idempotencyKey = ulid();
      return captureIntent<apis.auth.ConnectionsKickInput>({
        operation: "connectionsKick",
        input: {
          connectionId: connection.connectionId,
          idempotencyKey,
          reason: null,
        },
        label: describeSessionPrincipal(connection).title,
        targetId: connection.connectionId,
        scope: {
          routeKey: "/admin/sessions",
        },
        idempotencyKey,
      });
    });
    const confirmed = await confirmationModal?.confirm({
      title: `Disconnect ${targets.length} connection${targets.length === 1 ? "" : "s"}?`,
      message: "Each live NATS connection is dropped. Sessions stay valid until they reconnect.",
      confirmLabel: `Disconnect ${targets.length}`,
      targetLabel: "Connections",
      targetName: `${targets.length} connections`,
      expectedValue: bulkExpectedCount(targets.length),
      details: bulkTargetDetails(intents.map((intent) => intent.label)),
    });
    if (
      !confirmed || disposed || uncertain || intents.some((intent) =>

        !isIntentCurrent(intent, {
          routeKey: "/admin/sessions",
        })
      )
    ) return;
    await kickConnections(intents);
  }

  onMount(() => {
    loadActive();
    return () => { disposed = true; };
  });
</script>

<section class="space-y-4">
  <PageToolbar title="Sessions" description="Inspect active sessions and connections and disconnect compromised principals.">
    {#snippet actions()}
      <button class="btn btn-ghost btn-sm" onclick={loadActive} disabled={loading}>Refresh</button>
      <ActionMenu buttonBaseClass="btn btn-outline btn-sm" menuClass="z-10" widthClass="w-56">
        {#snippet summary()}
          Actions <Icon name="chevronDown" size={14} />
        {/snippet}
        <li><a href={resolve("/admin/sessions/revoke")}>Revoke a session</a></li>
        <li><a href={resolve("/admin/sessions/kick")}>Kick a connection</a></li>
      </ActionMenu>
    {/snippet}
  </PageToolbar>

  <div class="flex items-center justify-between">
    <div role="tablist" class="tabs tabs-bordered">
      <button
        role="tab"
        class={["tab", activeTab === "sessions" && "tab-active"]}
        onclick={() => { activeTab = "sessions"; void loadSessions(); }}
      >Sessions</button>
      <button
        role="tab"
        class={["tab", activeTab === "connections" && "tab-active"]}
        onclick={() => { activeTab = "connections"; void loadConnections(); }}
      >Connections</button>
    </div>
  </div>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if unknownLabels.length > 0}
    <Notice variant="warning">
      Outcome unknown for {unknownLabels.join(", ")}. The target may already be revoked or disconnected. Inspect the table and reload before any further action.
    </Notice>
  {/if}

  {#if activeTab === "sessions"}
    <form class="flex items-end gap-2" onsubmit={(e) => { e.preventDefault(); applySessionFilter(); }}>
      <input
        class="input input-bordered input-sm w-60 trellis-identifier"
        placeholder="Filter by principal ID"
        aria-label="Filter by principal ID"
        bind:value={sessionFilterUser}
      />
      <button type="submit" class="btn btn-outline btn-sm" disabled={sessionsLoading}>Apply</button>
      {#if sessionFilterUser.trim()}
        <button
          type="button"
          class="btn btn-ghost btn-sm"
          onclick={() => { sessionFilterUser = ""; applySessionFilter(); }}
        >Clear</button>
      {/if}
    </form>

    {#if loading}
      <Panel><LoadingState label="Loading sessions" /></Panel>
    {:else if sessions.length === 0}
      <EmptyState title="No sessions" description="No sessions match the current filter." />
    {:else}
      <Panel title="Sessions" eyebrow="Primary table">
        {#if sessionResult}
          <BulkResult
            succeeded={sessionResult.succeeded}
            failed={sessionResult.failed}
            pastTense="sessions revoked"
            onDismiss={() => { sessionResult = null; }}
          />
        {:else if selectedSessions.size > 0}
          <BulkActionBar count={selectedSessions.size} noun="session" onClear={() => selectedSessions.clear()}>
            {#snippet actions()}
              <button class="btn btn-error btn-outline btn-sm" disabled={bulkBusy || uncertain} onclick={() => void requestBulkRevoke()}>
                {bulkBusy ? "Revoking…" : "Revoke selected"}
              </button>
            {/snippet}
          </BulkActionBar>
        {/if}
        <DataTable fixed tableClass="w-full">
          <colgroup>
            <col class="w-10" />
            <col class="w-[40%]" />
            <col class="w-28" />
            <col class="w-36" />
            <col class="w-52" />
            <col class="w-28" />
          </colgroup>
          <thead>
            <tr>
              <th>
                <span class="sr-only">Select all sessions</span>
                <input
                  type="checkbox"
                  class="checkbox checkbox-xs"
                  aria-label="Select all sessions"
                  disabled={bulkBusy || selectableSessionIds.length === 0}
                  checked={selectableSessionIds.length > 0 && selectableSessionIds.every((id) => selectedSessions.has(id))}
                  indeterminate={selectableSessionIds.some((id) => selectedSessions.has(id)) && !selectableSessionIds.every((id) => selectedSessions.has(id))}
                  onchange={() => toggleAll(selectedSessions, selectableSessionIds)}
                />
              </th>
              <th>Principal</th>
              <th>Kind</th>
              <th><Term term="session key" /></th>
              <th>Activity</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each sessions as session (session.sessionId)}
              {@const summary = describeSessionPrincipal(session)}
              <tr>
                <td>
                  {#if !isCurrentSession(session)}
                    <input
                      type="checkbox"
                      class="checkbox checkbox-xs"
                      aria-label={`Select {summary.title}`}
                      disabled={bulkBusy}
                      checked={selectedSessions.has(session.sessionId)}
                      onchange={() => toggleId(selectedSessions, session.sessionId)}
                    />
                  {/if}
                </td>
                <td class="min-w-0">
                  <div class="truncate font-medium" title={summary.title}>{summary.title}</div>
                  {#if summary.details}
                    <div class="truncate text-xs text-base-content/60" title={summary.details}>{summary.details}</div>
                  {/if}
                </td>
                <td>
                  <span class={["badge badge-sm", participantKindBadgeClass(session.participantKind)]}>
                    {participantKindLabel(session.participantKind)}
                  </span>
                </td>
                <td class="trellis-identifier text-base-content/60">{formatShortKey(session.sessionKeyId)}</td>
                <td class="text-xs text-base-content/60">
                  <div>Last auth {formatDate(session.lastAuthenticatedAt)}</div>
                  <div>Created {formatDate(session.createdAt)}</div>
                </td>
                <td class="text-right">
                  <div class="flex items-center justify-end gap-2">
                    {#if isCurrentSession(session)}
                      <span class="badge badge-info badge-sm">Current</span>
                    {/if}
                    <ActionMenu menuClass="z-10" widthClass="w-48">
                      <li><a class="text-error" href={consoleUrl("/admin/sessions/revoke", { query: { sessionId: session.sessionId } })}>Revoke</a></li>
                    </ActionMenu>
                  </div>
                </td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
      <p class="text-xs text-base-content/50">{sessions.length} session{sessions.length !== 1 ? "s" : ""} on this page</p>
      </Panel>
    {/if}

  {:else}
    <form class="flex items-end gap-2" onsubmit={(e) => { e.preventDefault(); applyConnectionFilter(); }}>
      <input
        class="input input-bordered input-sm w-56 trellis-identifier"
        placeholder="Filter by session ID"
        aria-label="Filter by session ID"
        bind:value={connFilterSessionId}
      />
      <button type="submit" class="btn btn-outline btn-sm" disabled={connectionsLoading}>Apply</button>
      {#if connFilterSessionId.trim()}
        <button
          type="button"
          class="btn btn-ghost btn-sm"
          onclick={() => { connFilterSessionId = ""; applyConnectionFilter(); }}
        >Clear</button>
      {/if}
    </form>

    {#if loading}
      <Panel><LoadingState label="Loading connections" /></Panel>
    {:else if connections.length === 0}
      <EmptyState title="No connections" description="No active connections match the current filter." />
    {:else}
      <Panel title="Connections" eyebrow="Primary table">
        {#if connectionResult}
          <BulkResult
            succeeded={connectionResult.succeeded}
            failed={connectionResult.failed}
            pastTense="connections disconnected"
            onDismiss={() => { connectionResult = null; }}
          />
        {:else if selectedConnections.size > 0}
          <BulkActionBar count={selectedConnections.size} noun="connection" onClear={() => selectedConnections.clear()}>
            {#snippet actions()}
              <button class="btn btn-error btn-outline btn-sm" disabled={bulkBusy || uncertain} onclick={() => void requestBulkKick()}>
                {bulkBusy ? "Disconnecting…" : "Disconnect selected"}
              </button>
            {/snippet}
          </BulkActionBar>
        {/if}
      <DataTable>
          <thead>
            <tr>
              <th>
                <span class="sr-only">Select all connections</span>
                <input
                  type="checkbox"
                  class="checkbox checkbox-xs"
                  aria-label="Select all connections"
                  disabled={bulkBusy || selectableConnectionIds.length === 0}
                  checked={selectableConnectionIds.length > 0 && selectableConnectionIds.every((id) => selectedConnections.has(id))}
                  indeterminate={selectableConnectionIds.some((id) => selectedConnections.has(id)) && !selectableConnectionIds.every((id) => selectedConnections.has(id))}
                  onchange={() => toggleAll(selectedConnections, selectableConnectionIds)}
                />
              </th>
              <th>Principal</th>
              <th>Kind</th>
              <th>Session Key</th>
              <th>User NKey</th>
              <th>Server</th>
              <th>Connected</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each connections as connection (connection.connectionId)}
              {@const summary = describeSessionPrincipal(connection)}
              <tr>
                <td>
                  {#if connection.connectionId !== authority.identity.connectionId}
                    <input
                      type="checkbox"
                      class="checkbox checkbox-xs"
                      aria-label={`Select {summary.title}`}
                      disabled={bulkBusy}
                      checked={selectedConnections.has(connection.connectionId)}
                      onchange={() => toggleId(selectedConnections, connection.connectionId)}
                    />
                  {/if}
                </td>
                <td>
                  <div class="font-medium">{summary.title}</div>
                  {#if summary.details}
                    <div class="text-xs text-base-content/60">{summary.details}</div>
                  {/if}
                </td>
                <td>
                  <span class="badge badge-sm">Connection</span>
                </td>
                <td class="trellis-identifier text-base-content/60">{formatShortKey(connection.loginSessionId ?? "native")}</td>
                <td class="trellis-identifier text-base-content/60">{formatShortKey(connection.connectionId)}</td>
                <td>
                  <span class="text-sm">{connection.serverId}</span>
                  <span class="text-xs text-base-content/50 block">client {connection.clientId}</span>
                </td>
                <td class="text-base-content/60">{formatDate(connection.connectedAt)}</td>
                <td class="text-right">
                  <ActionMenu menuClass="z-10" widthClass="w-48">
                      <li><a class="text-error" href={consoleUrl("/admin/sessions/kick", { query: { connectionId: connection.connectionId } })}>Kick</a></li>
                  </ActionMenu>
                </td>
              </tr>
            {/each}
          </tbody>
      </DataTable>
      <p class="text-xs text-base-content/50">{connections.length} connection{connections.length !== 1 ? "s" : ""}</p>
      </Panel>
    {/if}
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
