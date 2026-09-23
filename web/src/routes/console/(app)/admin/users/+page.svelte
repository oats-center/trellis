<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oats-center/result";
  import { type apis } from "trellis-web-generated";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import { onMount } from "svelte";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import { nextCursorPage, previousCursorPage, resetCursorHistory } from "$lib/cursor_history.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  type ResetInput = Parameters<typeof trellis.usersPasswordResetCreate>[0];
  const resetMutation = new MutationController<ResetInput, apis.auth.UsersPasswordResetCreateOutput>();

  type UserView = apis.auth.UsersListOutput["items"][number];
  type PasswordResetResult = {
    userId: string;
    label: string;
    resetUrl: string;
    expiresAt: bigint;
  };

  // Identity details belong to the current user's own profile: the current
  // self-identity API cannot inspect another user, so this list does not
  // fabricate counts, providers, or usernames.
  function identityLabel(user: UserView): string {
    return user.name?.trim() || user.email?.trim() || user.userId;
  }

  let loading = $state(true);
  let error = $state<string | null>(null);
  let users = $state.raw<UserView[]>([]);
  let nextCursor = $state<string | undefined>(undefined);
  let cursorHistory = $state<{ cursor?: string; back: string[] }>({ back: [] });
  let search = $state("");
  let stateFilter = $state<"" | "active" | "disabled" | "revoked">("");
  let resetPendingUserId = $state<string | null>(null);
  let resetResult = $state<PasswordResetResult | null>(null);
  let resetDialog = $state<HTMLDialogElement | null>(null);

  const activeUserCount = $derived(users.filter((user) => user.state === "active").length);
  const inactiveUserCount = $derived(users.length - activeUserCount);

  async function load() {
    loading = true;
    error = null;
    try {
      const usersResponse = await trellis.usersList({
        ...(search.trim() === "" ? {} : { search: search.trim() }),
        ...(stateFilter === "" ? {} : { state: stateFilter }),
        page: {
          limit: TABLE_PAGE_LIMIT,
          ...(cursorHistory.cursor === undefined ? {} : { cursor: cursorHistory.cursor }),
        },
      }).take();
      if (isErr(usersResponse)) { error = errorMessage(usersResponse); return; }
      users = usersResponse.items ?? [];
      nextCursor = usersResponse.page.nextCursor;
    } catch (e) { error = errorMessage(e); }
    finally { loading = false; }
  }

  function applyFilters(): void {
    cursorHistory = resetCursorHistory();
    void load();
  }

  function goNext(): void {
    if (nextCursor === undefined) return;
    cursorHistory = nextCursorPage(cursorHistory, nextCursor);
    void load();
  }

  function goPrevious(): void {
    cursorHistory = previousCursorPage(cursorHistory);
    void load();
  }

  async function createPasswordReset(user: UserView) {
    if (resetPendingUserId) return;
    const key = ulid();
    const intent = captureIntent<ResetInput>({
      operation: "usersPasswordResetCreate", targetId: user.userId, label: identityLabel(user),
      idempotencyKey: key,
      input: { idempotencyKey: key, returnTarget: null, userId: user.userId },
      scope: { routeKey: "users-list" },
    });
    if (!resetMutation.begin(intent)) return;
    resetPendingUserId = user.userId;
    resetResult = null;
    const outcome = await resetMutation.send({
      isStillValid: () =>
        resetPendingUserId === intent.targetId,
      dispatch: async ({ input }) => await trellis.usersPasswordResetCreate(input).orThrow(),
    });
    resetPendingUserId = null;
    if (!outcome) return;
    if (outcome.kind === "succeeded") {
      resetResult = {
        userId: intent.targetId,
        label: intent.label,
        resetUrl: outcome.value.flow.completionUrl,
        expiresAt: outcome.value.flow.expiresAt,
      };
      resetDialog?.showModal();
      notifications.success(`Created password reset link for ${intent.label}.`, "Password reset ready");
    } else if (outcome.kind === "unknown") {
      notifications.error("Password reset outcome unknown; the one-time URL may not be recoverable. Check this user before trying again.", "Check outcome");
    } else {
      notifications.error(errorMessage(outcome.error), "Password reset failed");
    }
  }

  async function copyResetUrl() {
    if (!resetResult) return;
    if (typeof navigator === "undefined" || !navigator.clipboard) {
      notifications.error("Clipboard access is unavailable in this browser.", "Copy failed");
      return;
    }

    try {
      await navigator.clipboard.writeText(resetResult.resetUrl);
      notifications.success("Password reset URL copied to clipboard.", "Copied");
    } catch (e) {
      notifications.error(errorMessage(e), "Copy failed");
    }
  }

  function resetDialogAttachment(element: HTMLDialogElement) {
    resetDialog = element;
    return () => {
      if (resetDialog === element) resetDialog = null;
    };
  }

  onMount(() => { void load(); });
</script>

<section class="space-y-4">
  <PageToolbar title="Users" description="Manage user activation and capabilities.">
    {#snippet actions()}
      <a class="btn btn-outline btn-sm" href={resolve("/admin/users/new")}>New user</a>
      <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading}>Refresh</button>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if loading && users.length === 0}
    <Panel><LoadingState label="Loading users" /></Panel>
  {:else if users.length === 0}
    <EmptyState title="No users" description="No users match the current filters." />
  {:else}
    <div class="space-y-2">
      <div class="flex flex-col gap-3 border-y border-base-300 bg-base-100/45 px-3 py-3 sm:flex-row sm:items-center sm:justify-between sm:px-4">
        <div class="min-w-0">
          <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">User registry</p>
          <p class="mt-1 text-sm text-base-content/60">Account identity, activity, and activation state.</p>
        </div>
        <div class="flex shrink-0 flex-wrap items-center gap-2">
          <label class="input input-bordered input-sm flex items-center gap-2">
            <Icon name="search" size={14} class="text-base-content/50" />
            <input
              bind:value={search}
              class="grow"
              placeholder="Search users"
              aria-label="Search users"
              onkeydown={(event) => { if (event.key === "Enter") applyFilters(); }}
            />
          </label>
          <label class="select select-bordered select-sm">
            <select
              aria-label="Filter by state"
              value={stateFilter}
              onchange={(event) => {
                stateFilter = event.currentTarget.value as "" | "active" | "disabled" | "revoked";
                applyFilters();
              }}
            >
              <option value="">All states</option>
              <option value="active">Active</option>
              <option value="disabled">Disabled</option>
              <option value="revoked">Revoked</option>
            </select>
          </label>
          <span class="badge badge-success badge-sm">{activeUserCount} active on this page</span>
          {#if inactiveUserCount > 0}
            <span class="badge badge-neutral badge-sm">{inactiveUserCount} not active on this page</span>
          {/if}
          <span class="text-xs text-base-content/50">{users.length} on this page</span>
        </div>
      </div>

      <DataTable class="users-table border-b border-base-300 bg-base-100/30" overflow="visible">
        <thead>
          <tr>
            <th class="w-[28%]">Name</th>
            <th class="hidden w-[30%] md:table-cell">Email</th>
            <th class="hidden w-[16%] lg:table-cell">Updated</th>
            <th class="w-[8rem]">Status</th>
            <th class="w-[5rem] text-right">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each users as user (user.userId)}
            <tr class={["users-row", user.state !== "active" && "users-row-inactive"]}>
              <td class="max-w-0 align-top">
                <div class="flex min-w-0 items-start gap-3">
                  <span class={["mt-1.5 size-2 rounded-full", user.state === "active" ? "bg-success" : "bg-base-content/25"]} aria-hidden="true"></span>
                  <div class="min-w-0 space-y-1">
                    <div class="flex min-w-0 items-center gap-2">
                       <span class="truncate font-medium" title={identityLabel(user)}>{identityLabel(user)}</span>
                    </div>
                  </div>
                </div>
              </td>
              <td class="hidden max-w-0 align-top md:table-cell">
                <span class="block truncate text-xs text-base-content/70" title={user.email ?? "Not set"}>{user.email ?? "Not set"}</span>
                <span class="trellis-identifier mt-1 block truncate text-[0.68rem] text-base-content/40" title={user.userId}>{user.userId}</span>
              </td>
              <td class="hidden w-36 align-top text-xs text-base-content/60 lg:table-cell">
                {formatDate(user.updatedAt)}
              </td>
              <td class="w-28 align-top">
                {#if user.state === "active"}
                  <span class="badge badge-success badge-sm">Active</span>
                {:else}
                  <span class="badge badge-neutral badge-sm">Inactive</span>
                {/if}
              </td>
              <td class="w-24 whitespace-nowrap text-right align-top">
                <ActionMenu widthClass="w-44">
                    <li><a href={consoleUrl("/admin/users/edit", { query: { userId: user.userId } })}>Edit</a></li>
                     <li><button type="button" onclick={() => void createPasswordReset(user)} disabled={resetPendingUserId !== null}>{resetPendingUserId === user.userId ? "Creating reset..." : "Create reset link"}</button></li>
                </ActionMenu>
              </td>
            </tr>
          {/each}
        </tbody>
      </DataTable>
      <p class="text-xs text-base-content/50">{users.length} user{users.length !== 1 ? "s" : ""}</p>
    </div>
  {/if}
</section>

<dialog class="modal" {@attach resetDialogAttachment} onclose={() => { resetResult = null; resetMutation.reset(); }}>
  <div class="modal-box max-w-2xl border border-base-300 bg-base-100 p-0">
    {#if resetResult}
      <form method="dialog" class="absolute right-3 top-3">
        <button class="btn btn-ghost btn-xs btn-square" aria-label="Close password reset link dialog">x</button>
      </form>
      <div class="border-b border-base-300 px-5 py-4">
        <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">Password reset link</p>
        <dl class="mt-3 grid gap-3 text-sm sm:grid-cols-2">
          <div class="min-w-0">
            <dt class="text-xs font-medium uppercase tracking-wide text-base-content/50">User</dt>
            <dd class="truncate font-medium" title={resetResult.label}>{resetResult.label}</dd>
          </div>
          <div class="min-w-0">
            <dt class="text-xs font-medium uppercase tracking-wide text-base-content/50">User ID</dt>
            <dd class="trellis-identifier truncate" title={resetResult.userId}>{resetResult.userId}</dd>
          </div>
        </dl>
        <p class="trellis-field-help mt-1">Expires {formatDate(resetResult.expiresAt)}.</p>
      </div>
      <div class="px-5 py-4">
        <p class="trellis-field-help">Copy and send this URL to the user.</p>
        <input class="input input-bordered input-sm mt-3 w-full trellis-identifier" readonly value={resetResult.resetUrl} aria-label="Password reset URL" />
        <div class="mt-4 flex flex-wrap justify-end gap-2">
          <button class="btn btn-outline btn-sm" type="button" onclick={copyResetUrl}>Copy</button>
        </div>
      </div>
    {/if}
  </div>
  <form method="dialog" class="modal-backdrop">
    <button>close</button>
  </form>
</dialog>

<style>
  :global(.users-table) {
    min-width: 0;
    table-layout: fixed;
    width: 100%;
  }

  :global(.users-table thead) {
    background-color: color-mix(
      in oklab,
      var(--color-base-content) 3.5%,
      transparent
    );
  }

  .users-row {
    transition: background-color 160ms ease-out;
  }

  .users-row-inactive {
    background-color: color-mix(
      in oklab,
      var(--color-base-content) 2.5%,
      transparent
    );
  }

</style>
