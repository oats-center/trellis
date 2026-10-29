<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import { onMount } from "svelte";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import {
    nextCursorPage,
    previousCursorPage,
    resetCursorHistory,
  } from "$lib/cursor_history.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Pager from "$lib/components/Pager.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { traverseAll } from "$lib/console/paging.ts";
  import {
    getInitials,
    hasPlatformAdmin,
    isAdministrativeCapability,
  } from "$lib/control-panel.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const notifications = getNotifications();
  type ResetInput = Parameters<typeof trellis.usersPasswordResetCreate>[0];
  const resetMutation = new MutationController<
    ResetInput,
    apis.auth.UsersPasswordResetCreateOutput
  >();

  type UserView = apis.auth.UsersListOutput["items"][number];
  type PasswordResetResult = {
    userId: string;
    label: string;
    token: string;
    expiresAt: apis.auth.UsersPasswordResetCreateOutput["expiresAt"];
  };

  // User listings prefer the operator-facing local username (or display name)
  // over the stable principal/display identifier.
  function identityLabel(user: UserView): string {
    return user.account?.username || user.displayName;
  }

  let loading = $state(true);
  let error = $state<string | null>(null);
  let users = $state.raw<UserView[]>([]);
  let nextCursor = $state<string | undefined>(undefined);
  let cursorHistory = $state<{ cursor?: string; back: string[] }>({ back: [] });
  let search = $state("");
  let resetPendingUserId = $state<string | null>(null);
  let resetResult = $state<PasswordResetResult | null>(null);
  let resetDialog = $state<HTMLDialogElement | null>(null);
  let status = $state<"all" | "active" | "inactive">("all");
  // Role and administrator labels are optional context; a user who hasn't
  // allowed those reads still gets the user list.
  let rolesByUser = $state.raw<Map<string, string[]>>(new Map());
  let admins = $state.raw<Set<string>>(new Set());

  const visibleUsers = $derived(
    users.filter((user) => {
      const active = user.account?.principal.enabled ?? true;
      return status === "all" || (status === "active") === active;
    }),
  );

  const consoleAuthority = getConsoleAuthority();

  /** Administration is one of the six capabilities or a group that holds one. */
  const ADMINISTRATION_GROUP = "trellis.auth.v1:administration";
  function administers(capabilityId: string): boolean {
    return (
      isAdministrativeCapability(capabilityId) ||
      capabilityId === ADMINISTRATION_GROUP
    );
  }

  async function loadRoles(): Promise<void> {
    const [roles, assignments, grants] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis.rolesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.roleAssignmentsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.capabilityGrantsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    const titles = new Map(
      roles.items.map((role) => [role.roleId, role.title]),
    );
    const byUser = new Map<string, string[]>();
    for (const assignment of assignments.items) {
      const title = titles.get(assignment.roleId);
      if (!title) continue;
      byUser.set(assignment.principalId, [
        ...(byUser.get(assignment.principalId) ?? []),
        title,
      ]);
    }
    rolesByUser = byUser;
    // Administrators hold administration through a role or a direct grant.
    // The initial administrator holds it inherently; their own session shows it.
    const adminRoles = new Set(
      roles.items
        .filter((role) =>
          role.capabilities.some(({ capabilityId }) =>
            administers(capabilityId),
          ),
        )
        .map((role) => role.roleId),
    );
    const own = consoleAuthority.authority;
    const self = trellis.ownAuthority.current?.principalId;
    admins = new Set([
      ...assignments.items
        .filter((assignment) => adminRoles.has(assignment.roleId))
        .map((assignment) => assignment.principalId),
      ...grants.items
        .filter((grant) => administers(grant.capability.capabilityId))
        .map((grant) => grant.principalId),
      ...(self && hasPlatformAdmin(own) ? [self] : []),
    ]);
  }

  // Only the latest search or page request may update the list and cursor;
  // a slower earlier reply is discarded.
  let generation = 0;

  async function load() {
    const current = ++generation;
    loading = true;
    error = null;
    try {
      const usersResponse = await trellis
        .usersList({
          ...(search.trim() === "" ? {} : { search: search.trim() }),
          page: {
            limit: TABLE_PAGE_LIMIT,
            ...(cursorHistory.cursor === undefined
              ? {}
              : { cursor: cursorHistory.cursor }),
          },
        })
        .take();
      if (current !== generation) return;
      if (isErr(usersResponse)) {
        error = errorMessage(usersResponse);
        return;
      }
      users = usersResponse.items ?? [];
      nextCursor = usersResponse.page.nextCursor;
    } catch (e) {
      if (current === generation) error = errorMessage(e);
    } finally {
      if (current === generation) loading = false;
    }
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
      operation: "usersPasswordResetCreate",
      targetId: user.principalId,
      label: identityLabel(user),
      idempotencyKey: key,
      input: { idempotencyKey: key, userId: user.principalId },
      scope: { routeKey: "users-list" },
    });
    if (!resetMutation.begin(intent)) return;
    resetPendingUserId = user.principalId;
    resetResult = null;
    const outcome = await resetMutation.send({
      isStillValid: () => resetPendingUserId === intent.targetId,
      dispatch: async ({ input }) =>
        await trellis.usersPasswordResetCreate(input).orThrow(),
    });
    resetPendingUserId = null;
    if (!outcome) return;
    if (outcome.kind === "succeeded") {
      resetResult = {
        userId: intent.targetId,
        label: intent.label,
        token: outcome.value.token,
        expiresAt: outcome.value.expiresAt,
      };
      resetDialog?.showModal();
      notifications.success(
        `Created password reset link for ${intent.label}.`,
        "Password reset ready",
      );
    } else if (outcome.kind === "unknown") {
      notifications.error(
        "Password reset outcome unknown; the one-time URL may not be recoverable. Check this user before trying again.",
        "Check outcome",
      );
    } else {
      notifications.error(errorMessage(outcome.error), "Password reset failed");
    }
  }

  async function copyResetUrl() {
    if (!resetResult) return;
    if (typeof navigator === "undefined" || !navigator.clipboard) {
      notifications.error(
        "Clipboard access is unavailable in this browser.",
        "Copy failed",
      );
      return;
    }

    try {
      await navigator.clipboard.writeText(resetResult.token);
      notifications.success(
        "Password-reset token copied to clipboard.",
        "Copied",
      );
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

  onMount(() => {
    void load();
    void loadRoles();
  });
</script>

<section class="space-y-5">
  <PageToolbar title="Users" description="People who can sign in to Trellis.">
    {#snippet actions()}
      <a class="btn btn-primary" href={resolve("/admin/users/new")}
        ><Icon name="plus" size={18} />New user</a
      >
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  <div class="trellis-section">
    <div class="flex flex-wrap items-center gap-3 p-4">
      <label
        class="input flex w-full max-w-md items-center gap-2 bg-base-100 sm:w-auto sm:flex-1"
      >
        <Icon name="search" size={16} class="text-base-content/50" />
        <input
          bind:value={search}
          class="grow"
          placeholder="Search by name, username, or email"
          aria-label="Search users"
          onkeydown={(event) => {
            if (event.key === "Enter") applyFilters();
          }}
        />
      </label>
      <select
        class="select w-auto bg-base-100"
        aria-label="Filter by status"
        bind:value={status}
      >
        <option value="all">All statuses</option>
        <option value="active">Active</option>
        <option value="inactive">Inactive</option>
      </select>
    </div>

    {#if loading && users.length === 0}
      <LoadingState label="Loading users" />
    {:else if visibleUsers.length === 0}
      <EmptyState
        class="m-4"
        title="No users found"
        description={search || status !== "all"
          ? "Try a different search or filter."
          : "Add the first person who should sign in."}
      />
    {:else}
      <DataTable class="users-table" overflow="visible">
        <thead>
          <tr>
            <th class="w-[28%]">Name</th>
            <th class="hidden w-[18%] md:table-cell">Username</th>
            <th class="hidden w-[24%] lg:table-cell">Email</th>
            <th class="hidden w-[18%] md:table-cell">Roles</th>
            <th class="w-[7.5rem]">Status</th>
            <th class="w-[3.5rem]"><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each visibleUsers as user (user.principalId)}
            {@const active = user.account?.principal.enabled ?? true}
            {@const roles = [
              ...new Set([
                ...(admins.has(user.principalId) ? ["Administrator"] : []),
                ...(rolesByUser.get(user.principalId) ?? []),
              ]),
            ]}
            <tr class="users-row">
              <td class="max-w-0">
                <div class="flex min-w-0 items-center gap-3">
                  <span
                    class="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-primary text-sm font-medium text-primary-content"
                    aria-hidden="true"
                    >{getInitials({ displayName: user.displayName })}</span
                  >
                  <span class="truncate font-medium" title={user.displayName}
                    >{user.displayName}</span
                  >
                </div>
              </td>
              <td class="hidden max-w-0 truncate md:table-cell"
                >{user.account?.username ?? "—"}</td
              >
              <td
                class="hidden max-w-0 truncate text-base-content/75 lg:table-cell"
                >{user.email ?? "—"}</td
              >
              <td class="hidden max-w-0 md:table-cell">
                <div class="flex flex-wrap gap-1">
                  {#each roles as role (role)}
                    <span class="badge badge-sm badge-neutral">{role}</span>
                  {/each}
                </div>
              </td>
              <td>
                <span
                  class={[
                    "badge badge-sm gap-1.5",
                    active ? "badge-success" : "badge-neutral",
                  ]}
                  ><span
                    class={[
                      "h-1.5 w-1.5 rounded-full",
                      active ? "bg-success" : "bg-base-content/40",
                    ]}
                  ></span>{active ? "Active" : "Inactive"}</span
                >
              </td>
              <td class="text-right">
                {#if user.account}<ActionMenu
                    widthClass="w-44"
                    ariaLabel={`Actions for ${user.displayName}`}
                  >
                    <li>
                      <a
                        href={consoleUrl("/admin/users/edit", {
                          query: { userId: user.principalId },
                        })}>Edit</a
                      >
                    </li>
                    <li>
                      <button
                        type="button"
                        onclick={() => void createPasswordReset(user)}
                        disabled={resetPendingUserId !== null}
                        >{resetPendingUserId === user.principalId
                          ? "Creating reset..."
                          : "Reset password"}</button
                      >
                    </li>
                  </ActionMenu>{/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </DataTable>
    {/if}
    <!-- Outside the results branch: an empty later page still needs Previous. -->
    {#if !(loading && users.length === 0)}
      <Pager
        summary={`Showing ${visibleUsers.length} user${visibleUsers.length === 1 ? "" : "s"}${nextCursor !== undefined ? " on this page" : ""}`}
        canPrevious={cursorHistory.back.length > 0}
        canNext={nextCursor !== undefined}
        busy={loading}
        onPrevious={goPrevious}
        onNext={goNext}
      />
    {/if}
  </div>
</section>

<dialog
  class="modal"
  {@attach resetDialogAttachment}
  onclose={() => {
    resetResult = null;
    resetMutation.reset();
  }}
>
  <div class="modal-box max-w-2xl border border-base-300 bg-base-100 p-0">
    {#if resetResult}
      <form method="dialog" class="absolute right-3 top-3">
        <button
          class="btn btn-ghost btn-xs btn-square"
          aria-label="Close password reset link dialog">x</button
        >
      </form>
      <div class="border-b border-base-300 px-5 py-4">
        <p
          class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45"
        >
          Password reset link
        </p>
        <dl class="mt-3 grid gap-3 text-sm sm:grid-cols-2">
          <div class="min-w-0">
            <dt
              class="text-xs font-medium uppercase tracking-wide text-base-content/50"
            >
              User
            </dt>
            <dd class="truncate font-medium" title={resetResult.label}>
              {resetResult.label}
            </dd>
          </div>
          <div class="min-w-0">
            <dt
              class="text-xs font-medium uppercase tracking-wide text-base-content/50"
            >
              User ID
            </dt>
            <dd class="trellis-identifier truncate" title={resetResult.userId}>
              {resetResult.userId}
            </dd>
          </div>
        </dl>
        <p class="trellis-field-help mt-1">
          Expires {formatDate(resetResult.expiresAt)}.
        </p>
      </div>
      <div class="px-5 py-4">
        <p class="trellis-field-help">
          Send this one-time code to {resetResult.label}. It works once.
        </p>
        <input
          class="input input-bordered input-sm mt-3 w-full trellis-identifier"
          readonly
          value={resetResult.token}
          aria-label="Password-reset token"
        />
        <div class="mt-4 flex flex-wrap justify-end gap-2">
          <button
            class="btn btn-outline btn-sm"
            type="button"
            onclick={copyResetUrl}>Copy</button
          >
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
</style>
