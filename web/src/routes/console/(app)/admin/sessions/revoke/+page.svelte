<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oats-center/result";
  import { goto } from "$app/navigation";
  import { resolve, consoleUrl } from "$lib/console_paths";
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
    participantKindLabel,
    type SessionRecord,
  } from "$lib/auth_display.ts";
  import { formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("session-revoke");
  type RevokeInput = { expectedVersion: SessionRecord["version"]; idempotencyKey: string; reason: null; sessionId: string };
  let pending = $state(false);
  const mutation = new MutationController<RevokeInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let sessions = $state.raw<SessionRecord[]>([]);
  let selectedSessionId = $state("");
  /** Set when a current-session revoke succeeded: the expected sign-out path. */
  let selfRevoked = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  // Canonical query name; the legacy `sessionKey` name is not accepted.
  const requestedSessionId = $derived(
    page.url.searchParams.get("sessionId") ?? "",
  );
  const selectedSession = $derived(
    sessions.find((session) => session.sessionId ===
      (requestedSessionId || selectedSessionId)) ?? null,
  );
  const selectedSessionIsCurrent = $derived(
    selectedSession?.sessionId === authority.identity.loginSessionId,
  );
  const requestedUnavailable = $derived(
    requestedSessionId !== "" && selectedSession === null && !selfRevoked && !error,
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
    sessions = [];
    selectedSessionId = "";
    try {
      if (requestedId !== "") {
        // This console has no Sessions.Get: resolve the exact session through
        // its existing List endpoint over complete cursor traversal.
        const resolved = await resolveExact<SessionRecord>(
          async (pageRequest) => {
            const response = await trellis.sessionsList({
              page: catalogPage(pageRequest.cursor),
            }).take();
            if (isErr(response)) throw response;
            return { items: response.items, cursor: response.page.nextCursor };
          },
          (session) => session.sessionId === requestedId,
        );
        if (!scope.isCurrent(token)) return;
        if (!resolved.complete) {
          error = failureFrom(resolved.error);
          return;
        }
        sessions = resolved.item === undefined ? [] : [resolved.item];
        return;
      }

      const catalog = await traverseAll<SessionRecord>(async (pageRequest) => {
        const response = await trellis.sessionsList({ page: pageRequest }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor ?? undefined };
      });
      if (!scope.isCurrent(token)) return;
      if (!catalog.complete) {
        error = failureFrom(catalog.error);
        return;
      }
      sessions = [...catalog.items];
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestRevokeSession(): Promise<void> {
    const target = selectedSession;
    if (!target || target.state !== "active" || pending) return;
    const summary = describeSessionPrincipal(target);
    const requestedAtCapture = requestedSessionId;
    const isCurrent = target.sessionId === authority.identity.loginSessionId;
    const key = ulid();
    const intent = captureIntent({
      operation: "sessionsRevoke", targetId: target.sessionId, label: summary.title,
      expectedValue: target.sessionId, idempotencyKey: key,
      input: { expectedVersion: target.version, idempotencyKey: key, reason: null, sessionId: target.sessionId },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: isCurrent ? "Revoke your current session?" : "Revoke session?",
      message: isCurrent
        ? "This is the session powering this console. Continuing revokes it and signs you out."
        : "This immediately invalidates the selected active session.",
      confirmLabel: isCurrent ? "Revoke and sign out" : "Revoke session",
      targetLabel: intent.label, targetName: intent.targetId, expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const owns = () => scope.key === intent.scope.routeKey && selectedSession?.sessionId === intent.targetId;

    const outcome = await mutation.send({
      isStillValid: owns,
      dispatch: async ({ input }) => await trellis.sessionsRevoke(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedSessionId !== requestedAtCapture) return;

    if (outcome.kind === "succeeded") {
      if (isCurrent) {
        selfRevoked = true;
        authority.clear();
        notifications.success("Current session revoked. Signing out.", "Revoked");
        window.location.href = resolve("/profile");
      } else {
        notifications.success(`Session revoked for ${intent.label}.`, "Revoked");
        await goto(resolve("/admin/sessions"));
      }
    } else if (outcome.kind === "unknown") {
      error = { message: "Outcome unknown; the revoke request may have completed. Check the session list before starting a new operation." };
    } else {
      error = failureFrom(outcome.error);
    }
  }

  $effect(() => {
    const requestedId = requestedSessionId;
    scope.setKey(`session-revoke:${requestedId}`);
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(requestedId));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Revoke session" description="Confirm and revoke one exact active session.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/sessions")}>Back to sessions</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
       <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={() => void load(requestedSessionId)}>Retry</button>
    </Notice>
  {/if}

  {#if loading && !error}
    <Panel><LoadingState label="Loading sessions" /></Panel>
  {:else if error}
    <Panel><p class="text-sm text-base-content/60">Session lookup is incomplete. Retry before selecting a target.</p></Panel>
  {:else if requestedUnavailable || sessions.length === 0}
    <EmptyState
      title="Session unavailable"
      description={requestedSessionId !== ""
        ? `No session matches '${requestedSessionId}'. It may already be revoked, or the link may be stale.`
        : "No active sessions are available to revoke."}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/sessions")}>Back to sessions</a>
      {/snippet}
    </EmptyState>
  {:else}
    <Panel title="Confirm revoke" eyebrow="Workflow">
      <div class="space-y-4">
        <label class="form-control gap-1">
          <span class="label-text text-xs">Session</span>
          {#if requestedSessionId !== ""}
            <input
              class="input input-bordered input-sm font-mono"
              value={requestedSessionId}
              readonly
              aria-label="Session"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedSessionId} required>
              <option value="" disabled>Select a session…</option>
              {#each sessions as session (session.sessionId)}
                <option value={session.sessionId}>
                  {describeSessionPrincipal(session).title} · {session.sessionId} · {session.state}
                </option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedSession}
          <div class="rounded-box border border-base-300 p-3 text-sm">
            <div class="font-medium">{describeSessionPrincipal(selectedSession).title}</div>
            <div class="trellis-identifier text-base-content/60">{selectedSession.sessionId}</div>
            <div class="text-base-content/60">
              Kind: {participantKindLabel(selectedSession.participantKind)} · key {formatShortKey(selectedSession.sessionKeyId)}
            </div>
            <div class="text-base-content/60">Created {formatDate(selectedSession.createdAt)}</div>
            <div class="trellis-identifier text-base-content/60">version {selectedSession.version}</div>
            {#if selectedSessionIsCurrent}
              <div class="mt-2"><span class="badge badge-warning badge-sm">This console's session</span></div>
            {/if}
          </div>
          {#if selectedSession.state !== "active"}
            <Notice variant="warning">This session is {selectedSession.state} and cannot be revoked again.</Notice>
          {/if}
        {/if}

        <div class="flex flex-wrap gap-2">
          <button
            class="btn btn-error btn-sm"
            onclick={requestRevokeSession}
              disabled={!selectedSession || selectedSession.state !== "active" || pending || !!error}
          >
            {pending
              ? "Revoking..."
              : selectedSessionIsCurrent
              ? "Revoke and sign out"
              : "Revoke session"}
          </button>
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/sessions")}>Cancel</a>
          {#if selectedSession}
            <a
              class="btn btn-ghost btn-sm"
              href={consoleUrl("/admin/sessions", { query: { tab: "connections", sessionId: selectedSession.sessionId } })}
            >View connections</a>
          {/if}
        </div>
      </div>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
