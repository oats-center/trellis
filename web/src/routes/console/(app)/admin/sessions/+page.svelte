<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { ulid } from "ulid";
  import { SvelteSet } from "svelte/reactivity";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { runBulk } from "$lib/bulk.ts";
  import { formatDate, humanizeName } from "$lib/format";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import BulkActionBar from "$lib/components/BulkActionBar.svelte";
  import BulkResult from "$lib/components/BulkResult.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";

  type Session = apis.auth.AuthorizationSessionsListOutput["items"][number];
  type Attachment = apis.auth.ConnectionsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const sessionScope = new RequestScope("authorization-sessions");
  const attachmentScope = new RequestScope("attachments");
  let sessions = $state.raw<Session[]>([]);
  let attachments = $state.raw<Attachment[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let attachmentError = $state<string | null>(null);
  let tab = $state<"sessions" | "connections">("sessions");
  let filter = $state<"active" | "ended">("active");
  let search = $state("");
  // Names are optional context; IDs are the fallback when a read isn't allowed.
  let userNames = $state.raw<Map<string, string>>(new Map());
  let deploymentNames = $state.raw<Map<string, string>>(new Map());

  const liveBySession = $derived.by(() => {
    const counts = new Map<string, Attachment[]>();
    for (const attachment of attachments)
      if (attachment.state === "admitted")
        counts.set(attachment.authorizationSessionId, [
          ...(counts.get(attachment.authorizationSessionId) ?? []),
          attachment,
        ]);
    return counts;
  });
  const visibleSessions = $derived(
    sessions
      .filter((session) =>
        filter === "active"
          ? session.state === "active"
          : session.state !== "active",
      )
      .filter((session) =>
        `${who(session)} ${using(session)}`
          .toLowerCase()
          .includes(search.trim().toLowerCase()),
      )
      .sort((left, right) => right.createdAt.localeCompare(left.createdAt)),
  );

  // One row per person (or service) and app, so repeat sign-ins don't flood
  // the list; actions apply to every sign-in in the row.
  type Group = { key: string; latest: Session; sessions: Session[] };
  const groups = $derived.by((): Group[] => {
    const byKey = new Map<string, Group>();
    for (const session of visibleSessions) {
      const key = `${session.principalId}:${
        session.binding.clientId ??
        session.binding.deploymentId ??
        session.binding.instanceId ??
        ""
      }`;
      const group = byKey.get(key);
      if (group) group.sessions.push(session);
      else byKey.set(key, { key, latest: session, sessions: [session] });
    }
    return [...byKey.values()];
  });

  function liveIn(group: Group): Attachment[] {
    return group.sessions.flatMap(
      (session) => liveBySession.get(session.authorizationSessionId) ?? [],
    );
  }

  function who(session: Session): string {
    if (session.principalKind === "user")
      return userNames.get(session.principalId) ?? "Unknown user";
    const deployment = session.binding.deploymentId
      ? deploymentNames.get(session.binding.deploymentId)
      : undefined;
    if (session.principalKind === "device")
      return `Device ${(session.binding.instanceId ?? session.principalId).slice(-6)}`;
    return (
      deployment ?? humanizeName(session.binding.participantId ?? "Service")
    );
  }

  function using(session: Session): string {
    if (session.binding.clientId) return humanizeName(session.binding.clientId);
    if (session.binding.deploymentId)
      return deploymentNames.get(session.binding.deploymentId) ?? "—";
    return "—";
  }

  function ago(iso: string): string {
    const minutes = Math.floor((Date.now() - new Date(iso).getTime()) / 60000);
    if (!Number.isFinite(minutes)) return "—";
    if (minutes < 1) return "just now";
    if (minutes < 60) return `${minutes} min ago`;
    const hours = Math.floor(minutes / 60);
    return hours < 24 ? `${hours} h ago` : `${Math.floor(hours / 24)} d ago`;
  }

  async function loadNames(): Promise<void> {
    const [users, deployments] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis.usersList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.deploymentsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    userNames = new Map(
      users.items.map((user) => [user.principalId, user.displayName]),
    );
    deploymentNames = new Map(
      deployments.items.map((item) => [
        item.deploymentId,
        humanizeName(item.title),
      ]),
    );
  }

  async function disconnect(group: Group): Promise<void> {
    const live = liveIn(group);
    if (busy || !live.length) return;
    const confirmed = await confirmationModal?.confirm({
      title: `Disconnect ${who(group.latest)}?`,
      message:
        "Their live connections close. They stay signed in and may reconnect.",
      confirmLabel: "Disconnect",
      targetLabel: "Who",
      targetName: `${who(group.latest)} · ${using(group.latest)}`,
    });
    if (!confirmed || sessionScope.disposed) return;
    await run(
      live.map((item) => item.attachmentId),
      (id, key) =>
        trellis
          .connectionsKick({ attachmentId: id, idempotencyKey: key })
          .orThrow(),
    );
  }

  async function signOut(group: Group): Promise<void> {
    const active = group.sessions.filter(
      (session) => session.state === "active",
    );
    if (busy || !active.length) return;
    const confirmed = await confirmationModal?.confirm({
      title: `Sign out ${who(group.latest)}?`,
      message:
        active.length === 1
          ? "They'll need to sign in again."
          : `All ${active.length} of their sign-ins in ${using(group.latest)} end. They'll need to sign in again.`,
      confirmLabel: "Sign out",
      targetLabel: "Who",
      targetName: `${who(group.latest)} · ${using(group.latest)}`,
    });
    if (!confirmed || sessionScope.disposed) return;
    await run(
      active.map((session) => session.authorizationSessionId),
      (id, key) =>
        trellis
          .authorizationSessionsRevoke({
            authorizationSessionId: id,
            idempotencyKey: key,
          })
          .orThrow(),
    );
  }

  async function run<Id>(
    ids: Id[],
    send: (id: Id, key: string) => Promise<unknown>,
  ): Promise<void> {
    busy = true;
    const outcome = await runBulk(
      ids.map((id) => ({ id, key: ulid() })),
      async (target) => {
        await send(target.id, target.key);
      },
      (cause) =>
        classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed",
    );
    if (sessionScope.disposed) return;
    result = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((item) => item.reason),
    };
    uncertain = outcome.unknown.length > 0;
    busy = false;
    await load();
  }
  const selected = new SvelteSet<string>();
  let busy = $state(false);
  let uncertain = $state(false);
  let result = $state<{ succeeded: number; failed: string[] } | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();

  async function load(): Promise<void> {
    const st = sessionScope.begin();
    const at = attachmentScope.begin();
    loading = true;
    const [sessionResult, attachmentResult] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis
          .authorizationSessionsList({ page })
          .orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.connectionsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    if (sessionScope.settle(st)) {
      sessions = [...sessionResult.items];
      error = sessionResult.complete
        ? null
        : projectConsoleError(sessionResult.error).message;
      loading = false;
    }
    if (attachmentScope.settle(at)) {
      attachments = [...attachmentResult.items];
      attachmentError = attachmentResult.complete
        ? null
        : projectConsoleError(attachmentResult.error).message;
    }
  }

  async function bulkAction(): Promise<void> {
    const operation = tab;
    if (
      busy ||
      uncertain ||
      !canPerform(
        authority.authority,
        operation === "sessions"
          ? "authorizationSessionsRevoke"
          : "connectionsKick",
      )
    )
      return;
    const targets =
      operation === "sessions"
        ? sessions
            .filter(
              (item) =>
                selected.has(item.authorizationSessionId) &&
                item.state === "active",
            )
            .map((item) => ({ id: item.authorizationSessionId, key: ulid() }))
        : attachments
            .filter(
              (item) =>
                selected.has(item.attachmentId) && item.state === "admitted",
            )
            .map((item) => ({ id: item.attachmentId, key: ulid() }));
    if (!targets.length) return;
    const confirmed = await confirmationModal?.confirm({
      title:
        operation === "sessions"
          ? `Sign out ${targets.length} session${targets.length === 1 ? "" : "s"}?`
          : "Disconnect selected?",
      message:
        operation === "sessions"
          ? "They're signed out and disconnected. They'll need to sign in again."
          : "Live connections close. They stay signed in and may reconnect.",
      confirmLabel: operation === "sessions" ? "Sign out" : "Disconnect",
      targetLabel: "Sessions",
      targetName: `${targets.length} sign-in${targets.length === 1 ? "" : "s"}`,
    });
    if (!confirmed || tab !== operation || sessionScope.disposed) return;
    busy = true;
    const outcome = await runBulk(
      targets,
      async (target) => {
        if (sessionScope.disposed || tab !== operation)
          throw new Error("Page target changed before dispatch.");
        if (operation === "sessions")
          await trellis
            .authorizationSessionsRevoke({
              authorizationSessionId: target.id,
              idempotencyKey: target.key,
            })
            .orThrow();
        else
          await trellis
            .connectionsKick({
              attachmentId: target.id,
              idempotencyKey: target.key,
            })
            .orThrow();
      },
      (cause) =>
        classifyMutationError(cause).kind === "unknown" ? "unknown" : "failed",
    );
    if (sessionScope.disposed) return;
    result = {
      succeeded: outcome.succeeded,
      failed: outcome.failed.map((item) => `${item.target.id}: ${item.reason}`),
    };
    uncertain = outcome.unknown.length > 0;
    busy = false;
    selected.clear();
    await load();
  }
  onMount(() => {
    void load();
    void loadNames().catch(() => {});
    return () => {
      sessionScope.dispose();
      attachmentScope.dispose();
    };
  });
</script>

<section class="space-y-5">
  <PageToolbar
    title="Sessions"
    description="Who is signed in, and what is connected right now."
  />
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if attachmentError}<Notice variant="warning"
      >Live connection status may be incomplete: {attachmentError}</Notice
    >{/if}
  {#if uncertain}<Notice variant="warning"
      >We couldn't confirm every result. Check the list before trying again.</Notice
    >{/if}
  {#if result}<BulkResult
      succeeded={result.succeeded}
      failed={result.failed}
      pastTense="done"
      onDismiss={() => {
        result = null;
      }}
    />{/if}

  <div class="trellis-section">
    <div class="flex flex-wrap items-center gap-3 p-4">
      <div class="trellis-segment" role="group" aria-label="Filter sessions">
        <button
          type="button"
          class={{ active: filter === "active" }}
          aria-pressed={filter === "active"}
          onclick={() => {
            filter = "active";
            selected.clear();
          }}>Signed in</button
        >
        <button
          type="button"
          class={{ active: filter === "ended" }}
          aria-pressed={filter === "ended"}
          onclick={() => {
            filter = "ended";
            selected.clear();
          }}>Ended</button
        >
      </div>
      <label
        class="input flex w-full max-w-md items-center gap-2 bg-base-100 sm:w-auto sm:flex-1"
      >
        <Icon name="search" size={16} class="text-base-content/50" />
        <input
          class="grow"
          bind:value={search}
          placeholder="Search by person, service, or app"
          aria-label="Search sessions"
        />
      </label>
    </div>

    {#if selected.size}<div class="px-4">
        <BulkActionBar
          count={selected.size}
          noun="sign-in"
          onClear={() => selected.clear()}
          >{#snippet actions()}<button
              class="btn btn-error btn-outline btn-sm"
              disabled={busy || uncertain}
              onclick={() => void bulkAction()}>Sign out</button
            >{/snippet}</BulkActionBar
        >
      </div>{/if}

    {#if loading}
      <LoadingState label="Loading sessions" />
    {:else if !visibleSessions.length}
      <EmptyState
        class="m-4"
        title={filter === "active"
          ? "Nobody is signed in"
          : "No ended sessions"}
        description={search ? "Try a different search." : undefined}
      />
    {:else}
      <DataTable>
        <thead>
          <tr>
            <th class="w-10"><span class="sr-only">Select</span></th>
            <th>Who</th>
            <th>Using</th>
            <th>Signed in</th>
            <th>Connected</th>
            <th>Expires</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each groups as group (group.key)}
            {@const session = group.latest}
            {@const live = liveIn(group).length}
            {@const ids = group.sessions
              .filter((item) => item.state === "active")
              .map((item) => item.authorizationSessionId)}
            <tr>
              <td>
                <input
                  class="checkbox checkbox-sm"
                  type="checkbox"
                  checked={ids.length > 0 &&
                    ids.every((id) => selected.has(id))}
                  aria-label={`Select ${who(session)}`}
                  disabled={busy || !ids.length}
                  onchange={(event) => {
                    for (const id of ids)
                      if (event.currentTarget.checked) selected.add(id);
                      else selected.delete(id);
                  }}
                />
              </td>
              <td>
                <div class="flex items-center gap-3">
                  <span
                    class="grid h-9 w-9 place-items-center rounded-full bg-base-200 text-base-content/70"
                    ><Icon
                      name={session.principalKind === "user"
                        ? "user"
                        : session.principalKind === "device"
                          ? "laptop"
                          : "box"}
                      size={17}
                    /></span
                  >
                  <span>
                    <span class="block font-medium">{who(session)}</span>
                    <span class="block text-xs text-base-content/60"
                      >{session.principalKind === "user"
                        ? "Person"
                        : session.principalKind === "device"
                          ? "Device"
                          : "Service"}{group.sessions.length > 1
                        ? ` · ${group.sessions.length} sign-ins`
                        : ""}</span
                    >
                  </span>
                </div>
              </td>
              <td class="text-base-content/80">{using(session)}</td>
              <td class="text-base-content/75">{ago(session.createdAt)}</td>
              <td>
                {#if live}
                  <span class="inline-flex items-center gap-2"
                    ><span class="h-2 w-2 rounded-full bg-success"
                    ></span>{live === 1 ? "Yes" : `${live} connections`}</span
                  >
                {:else}
                  <span class="text-base-content/55">No</span>
                {/if}
              </td>
              <td class="text-base-content/75"
                >{session.state !== "active"
                  ? session.state === "revoked"
                    ? "Signed out"
                    : "Replaced"
                  : session.notAfter
                    ? formatDate(session.notAfter)
                    : "—"}</td
              >
              <td class="text-right">
                {#if ids.length}
                  <ActionMenu
                    widthClass="w-44"
                    ariaLabel={`Actions for ${who(session)}`}
                  >
                    {#if live}
                      <li>
                        <button
                          type="button"
                          disabled={busy}
                          onclick={() => void disconnect(group)}
                          >Disconnect</button
                        >
                      </li>
                    {/if}
                    <li>
                      <button
                        type="button"
                        class="text-error"
                        disabled={busy}
                        onclick={() => void signOut(group)}>Sign out</button
                      >
                    </li>
                  </ActionMenu>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </DataTable>
      <div
        class="border-t border-base-300 px-4 py-3 text-sm text-base-content/60"
      >
        Showing {groups.length} of {visibleSessions.length} sign-in{visibleSessions.length ===
        1
          ? ""
          : "s"}
      </div>
    {/if}
  </div>
</section>
<ConfirmationModal bind:this={confirmationModal} />
