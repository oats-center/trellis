<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onDestroy, untrack } from "svelte";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { formatDate, errorMessage } from "$lib/format";
  import Panel from "./Panel.svelte";
  import Notice from "./Notice.svelte";
  import LoadingState from "./LoadingState.svelte";
  import CopyButton from "./CopyButton.svelte";

  type Identity = apis.auth.UserIdentitiesListOutput["items"][number];
  type Session = apis.auth.SessionsListOutput["items"][number];
  type Connection = apis.auth.ConnectionsListOutput["items"][number];
  type ResetFlow = apis.auth.UsersPasswordResetCreateOutput["flow"];
  type LinkFlow = apis.auth.UsersIdentityLinkCreateOutput["flow"];
  type Action = { kind: "unlink"; identity: Identity } | { kind: "revoke"; session: Session } | { kind: "kick"; connection: Connection };
  type Input = { kind: "unlink"; value: apis.auth.UserIdentitiesUnlinkInput } | { kind: "revoke"; value: apis.auth.SessionsRevokeInput } | { kind: "kick"; value: apis.auth.ConnectionsKickInput };
  type Output = apis.auth.UserIdentitiesUnlinkOutput | apis.auth.SessionsRevokeOutput | apis.auth.ConnectionsKickOutput;

  let { userId, userLabel, disabled = false, version }: { userId: string; userLabel: string; disabled?: boolean; version: bigint } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-security");
  const resetMutation = new MutationController<apis.auth.UsersPasswordResetCreateInput, apis.auth.UsersPasswordResetCreateOutput>((state) => resetPending = state.busy);
  const linkMutation = new MutationController<apis.auth.UsersIdentityLinkCreateInput, apis.auth.UsersIdentityLinkCreateOutput>((state) => linkPending = state.busy);
  const actionMutation = new MutationController<Input, Output>((state) => actionPending = state.busy);
  let resetPending = $state(false);
  let linkPending = $state(false);
  let actionPending = $state(false);
  let identities = $state.raw<Identity[]>([]);
  let sessions = $state.raw<Session[]>([]);
  let connections = $state.raw<Connection[]>([]);
  let resetFlow = $state.raw<ResetFlow | null>(null);
  let linkFlow = $state.raw<LinkFlow | null>(null);
  let identityError = $state<string | null>(null);
  let sessionError = $state<string | null>(null);
  let connectionError = $state<string | null>(null);
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);
  let loading = $state(true);
  let sessionLoading = $state(true);
  let connectionLoading = $state(true);
  let uncertain = $state(false);
  let action = $state.raw<Action | null>(null);
  let providerIds = $state("");
  let sessionGeneration = 0;
  let connectionGeneration = 0;
  const busy = $derived(disabled || resetPending || linkPending || actionPending || uncertain);

  async function loadIdentities(): Promise<void> {
    const token = scope.begin();
    const targetId = userId;
    loading = true;
    identityError = null;
    try {
      const result = await traverseAll<Identity>(async (request) => {
        const response = await trellis.userIdentitiesList({ userId: targetId, page: catalogPage(request.cursor) }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      if (!result.complete) throw result.error;
      identities = [...result.items];
    } catch (cause) { if (scope.isCurrent(token)) identityError = errorMessage(cause); }
    finally { if (scope.settle(token)) loading = false; }
  }

  async function loadSessions(): Promise<void> {
    const key = scope.key;
    const generation = ++sessionGeneration;
    const targetId = userId;
    sessionLoading = true;
    sessionError = null;
    try {
      const sessionResult = await traverseAll<Session>(async (request) => {
        const response = await trellis.sessionsList({ principalId: targetId, page: catalogPage(request.cursor) }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (scope.key !== key || sessionGeneration !== generation) return;
      if (!sessionResult.complete) throw sessionResult.error;
      sessions = [...sessionResult.items];
    } catch (cause) { if (scope.key === key && sessionGeneration === generation) sessionError = errorMessage(cause); }
    finally { if (scope.key === key && sessionGeneration === generation) sessionLoading = false; }
  }

  async function loadConnections(): Promise<void> {
    const key = scope.key;
    const generation = ++connectionGeneration;
    const targetId = userId;
    connectionLoading = true;
    connectionError = null;
    try {
      const result = await traverseAll<Connection>(async (request) => {
        const response = await trellis.connectionsList({ principalId: targetId, page: catalogPage(request.cursor) }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (scope.key !== key || connectionGeneration !== generation) return;
      if (!result.complete) throw result.error;
      connections = [...result.items];
    } catch (cause) { if (scope.key === key && connectionGeneration === generation) connectionError = errorMessage(cause); }
    finally { if (scope.key === key && connectionGeneration === generation) connectionLoading = false; }
  }

  async function createReset(): Promise<void> {
    if (busy || !canPerform(authority.authority, "usersPasswordResetCreate")) return;
    const key = ulid();
    const intent = captureIntent<apis.auth.UsersPasswordResetCreateInput>({ operation: "usersPasswordResetCreate", input: { userId, returnTarget: null, idempotencyKey: key }, targetId: userId, label: userLabel, idempotencyKey: key, scope: { routeKey: scope.key } });
    if (!resetMutation.begin(intent)) return;
    error = null;
    resetFlow = null;
    const outcome = await resetMutation.send({ isStillValid: () => scope.key === intent.scope.routeKey && !disabled, dispatch: ({ input }) => trellis.usersPasswordResetCreate(input).orThrow() });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") { resetFlow = outcome.value.flow; saved = "Password reset link created for this user."; }
    else if (outcome.kind === "unknown") { uncertain = true; error = "Password reset outcome unknown. The one-time URL may not be recoverable. Verify the user before creating another link."; }
    else error = errorMessage(outcome.error);
  }

  async function createLink(): Promise<void> {
    if (busy || !canPerform(authority.authority, "usersIdentityLinkCreate")) return;
    const key = ulid();
    const intent = captureIntent<apis.auth.UsersIdentityLinkCreateInput>({ operation: "usersIdentityLinkCreate", input: { userId, allowedProviders: providerIds.split(",").map((id) => id.trim()).filter(Boolean), returnTarget: null, idempotencyKey: key }, targetId: userId, label: userLabel, idempotencyKey: key, scope: { routeKey: scope.key } });
    if (!linkMutation.begin(intent)) return;
    error = null;
    linkFlow = null;
    const outcome = await linkMutation.send({ isStillValid: () => scope.key === intent.scope.routeKey && !disabled, dispatch: ({ input }) => trellis.usersIdentityLinkCreate(input).orThrow() });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") { linkFlow = outcome.value.flow; saved = "Sign-in-method link created for this user."; }
    else if (outcome.kind === "unknown") { uncertain = true; error = "Identity-link outcome unknown. Check before creating another link."; }
    else error = errorMessage(outcome.error);
  }

  async function performAction(): Promise<void> {
    const target = action;
    if (!target || busy) return;
    const operation = target.kind === "unlink" ? "userIdentitiesUnlink" : target.kind === "revoke" ? "sessionsRevoke" : "connectionsKick";
    if (!canPerform(authority.authority, operation)) return;
    const key = ulid();
    const input: Input = target.kind === "unlink" ? { kind: "unlink", value: { userId, providerId: target.identity.providerId, subject: target.identity.subject, idempotencyKey: key } } : target.kind === "revoke" ? { kind: "revoke", value: { sessionId: target.session.sessionId, expectedVersion: target.session.version, idempotencyKey: key, reason: "Ended from user management" } } : { kind: "kick", value: { connectionId: target.connection.connectionId, idempotencyKey: key, reason: "Disconnected from user management" } };
    const intent = captureIntent({ operation, input, targetId: userId, label: userLabel, idempotencyKey: key, scope: { routeKey: scope.key } });
    if (!actionMutation.begin(intent)) return;
    error = null;
    const outcome = await actionMutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && action === target && !disabled,
      dispatch: ({ input }) => input.kind === "unlink" ? trellis.userIdentitiesUnlink(input.value).orThrow() : input.kind === "revoke" ? trellis.sessionsRevoke(input.value).orThrow() : trellis.connectionsKick(input.value).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") { action = null; saved = "Security action completed. Refreshed details are shown below."; await Promise.all([loadIdentities(), loadSessions(), loadConnections()]); }
    else if (outcome.kind === "unknown") { uncertain = true; error = "Result unknown. Refresh security details to verify before retrying."; }
    else error = errorMessage(outcome.error);
  }

  $effect(() => {
    scope.setKey(JSON.stringify([userId, String(version)]));
    resetMutation.cancel(); linkMutation.cancel(); actionMutation.cancel();
    untrack(() => { identities = []; sessions = []; connections = []; resetFlow = null; linkFlow = null; action = null; error = null; saved = null; uncertain = false; void loadIdentities(); void loadSessions(); void loadConnections(); });
    return () => scope.invalidate();
  });
  onDestroy(() => { scope.dispose(); ++sessionGeneration; ++connectionGeneration; });
</script>

<Panel title="Sign-in & security">
  {#snippet actions()}<button class="btn btn-ghost btn-sm" disabled={resetPending || linkPending || actionPending || loading || sessionLoading || connectionLoading} onclick={async () => { await Promise.all([loadIdentities(), loadSessions(), loadConnections()]); if (!identityError && !sessionError && !connectionError) { uncertain = false; error = null; action = null; } }}>Refresh security</button>{/snippet}
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if saved}<Notice variant="success">{saved}</Notice>{/if}
  {#if action?.kind === "unlink"}{@render confirmation()}{/if}
  <div class="grid gap-5 lg:grid-cols-2">
    <section class="space-y-3">
      <h3 class="text-sm font-semibold">Password reset</h3><p class="trellis-field-help">Generate a one-time link for {userLabel}. Share it privately; the user chooses their own password.</p>
      <button type="button" class="btn btn-outline btn-sm" disabled={busy || !canPerform(authority.authority, "usersPasswordResetCreate")} onclick={() => void createReset()}>{resetPending ? "Generating…" : "Generate password reset link"}</button>
      {#if resetFlow}<div class="space-y-1"><p class="trellis-field-label">Reset link for {userLabel}</p><div class="flex items-start gap-2"><a class="link break-all text-xs" href={resetFlow.completionUrl} target="_blank" rel="noopener noreferrer">{resetFlow.completionUrl}</a><CopyButton value={resetFlow.completionUrl} label="Copy password reset link" /></div><p class="trellis-metadata">Expires {formatDate(resetFlow.expiresAt)} · One-time link</p></div>{/if}
    </section>
    <section class="space-y-3">
      <h3 class="text-sm font-semibold">Link another sign-in method</h3><p class="trellis-field-help">Give this user a secure link to connect an external identity. Do not complete it using your own provider account.</p>
      <label class="form-control"><span class="trellis-field-label">Allowed provider IDs (optional)</span><input class="input input-bordered input-sm mt-1" bind:value={providerIds} disabled={busy} placeholder="github, google" /><span class="trellis-field-help">Comma-separated configured provider IDs. Empty allows the portal's available providers.</span></label>
      <button type="button" class="btn btn-outline btn-sm" disabled={busy || !canPerform(authority.authority, "usersIdentityLinkCreate")} onclick={() => void createLink()}>{linkPending ? "Generating…" : "Generate sign-in-method link"}</button>
      {#if linkFlow}<div class="space-y-1"><p class="trellis-field-label">Sign-in-method link for {userLabel}</p><div class="flex items-start gap-2"><a class="link break-all text-xs" href={linkFlow.completionUrl} target="_blank" rel="noopener noreferrer">{linkFlow.completionUrl}</a><CopyButton value={linkFlow.completionUrl} label="Copy sign-in-method link" /></div><p class="trellis-metadata">Expires {formatDate(linkFlow.expiresAt)} · One-time link</p></div>{/if}
    </section>
  </div>
  <section class="border-t border-base-300 mt-5 pt-4">
    <h3 class="text-sm font-semibold mb-2">Linked sign-in methods</h3>
    {#if identityError}<Notice variant="error">{identityError}</Notice>{:else if loading}<LoadingState label="Loading sign-in methods" />{:else}
      <div class="overflow-x-auto"><table class="table table-sm"><thead><tr><th>Provider</th><th>Identity</th><th>Last sign-in</th><th><span class="sr-only">Actions</span></th></tr></thead><tbody>
        {#each identities as identity (`${identity.providerId}:${identity.subject}`)}<tr><td>{identity.providerId === "local" ? "Username & password" : identity.providerId}</td><td><span class="trellis-identifier break-all">{identity.username ?? identity.subject}</span><p class="trellis-metadata">{identity.observedEmail ?? identity.observedName ?? ""}</p></td><td class="text-xs">{formatDate(identity.lastSeenAt)}</td><td><button type="button" class="btn btn-ghost btn-sm text-error" disabled={busy || identity.providerId === "local" || identities.length <= 1 || !canPerform(authority.authority, "userIdentitiesUnlink")} onclick={() => action = { kind: "unlink", identity }}>Unlink</button></td></tr>
        {:else}<tr><td colspan="4" class="text-sm text-base-content/60">No linked sign-in methods.</td></tr>{/each}
      </tbody></table></div><p class="trellis-field-help mt-2">The final sign-in method cannot be removed. Edit a local username in Profile instead.</p>
    {/if}
  </section>
</Panel>

<Panel title="Sessions & connections">
  {#if action && action.kind !== "unlink"}{@render confirmation()}{/if}
  <p class="trellis-field-help mb-3">End session revokes a login. Disconnect closes only the selected physical connection; the session may reconnect.</p>
  {#if sessionError}<Notice variant="error">Could not load sessions: {sessionError}</Notice>{:else if sessionLoading}<LoadingState label="Loading sessions" />{:else}
    <div class="overflow-x-auto"><table class="table table-sm"><thead><tr><th>Application / session</th><th>Signed in / last authenticated</th><th>State / expiry</th><th><span class="sr-only">Actions</span></th></tr></thead><tbody>
      {#each sessions as session (session.sessionId)}<tr><td><span class="trellis-identifier">{session.participantId}</span><p class="trellis-metadata break-all">{session.sessionId}</p></td><td class="text-xs">{formatDate(session.createdAt)}<p class="trellis-metadata">{formatDate(session.lastAuthenticatedAt)}</p></td><td><span class="badge badge-sm">{session.state}</span><p class="trellis-metadata">{session.expiresAt === null ? "No expiry" : formatDate(session.expiresAt)}</p></td><td><button class="btn btn-ghost btn-sm text-error" disabled={busy || session.state !== "active" || !canPerform(authority.authority, "sessionsRevoke")} onclick={() => action = { kind: "revoke", session }}>End session</button></td></tr>
      {:else}<tr><td colspan="4" class="text-sm text-base-content/60">No sessions for this user.</td></tr>{/each}
    </tbody></table></div>
  {/if}
  <h3 class="text-sm font-semibold mt-4 mb-2">Open connections</h3>
  {#if connectionError}<Notice variant="error">Could not load connections: {connectionError}</Notice>{:else if connectionLoading}<LoadingState label="Loading connections" />{:else}
    <div class="overflow-x-auto"><table class="table table-sm"><thead><tr><th>Application / connection</th><th>Connected / last seen</th><th>Address</th><th><span class="sr-only">Actions</span></th></tr></thead><tbody>
      {#each connections as connection (connection.connectionId)}<tr><td><span class="trellis-identifier">{connection.participantId}</span><p class="trellis-metadata break-all">{connection.connectionId}</p></td><td class="text-xs">{formatDate(connection.connectedAt)}<p class="trellis-metadata">{formatDate(connection.lastSeenAt)}</p></td><td class="trellis-identifier">{connection.remoteAddress ?? "—"}</td><td><button class="btn btn-ghost btn-sm text-error" disabled={busy || !canPerform(authority.authority, "connectionsKick")} onclick={() => action = { kind: "kick", connection }}>Disconnect</button></td></tr>
      {:else}<tr><td colspan="4" class="text-sm text-base-content/60">No open connections.</td></tr>{/each}
    </tbody></table></div>
  {/if}
</Panel>

{#snippet confirmation()}
{#if action}
  <Notice variant="warning"><div class="space-y-2"><p class="font-medium">{action.kind === "unlink" ? `Unlink ${action.identity.providerId}:${action.identity.subject}` : action.kind === "revoke" ? `End session ${action.session.sessionId}` : `Disconnect ${action.connection.connectionId}`} for {userLabel}?</p><p class="text-xs">{action.kind === "unlink" ? "This removes the user's ability to sign in with this identity." : action.kind === "revoke" ? "The login and its active authorization will be revoked." : "The underlying login remains valid and may reconnect."}</p>{#if userId === authority.identity.principalId}<p class="text-xs">This is your own account. The action may disconnect you.</p>{/if}<div class="flex gap-2"><button class="btn btn-error btn-outline btn-sm" disabled={busy} onclick={() => void performAction()}>Confirm {action.kind === "unlink" ? "unlink" : action.kind === "revoke" ? "end session" : "disconnect"}</button><button class="btn btn-ghost btn-sm" disabled={actionPending} onclick={() => action = null}>Cancel</button></div></div></Notice>
{/if}
{/snippet}
