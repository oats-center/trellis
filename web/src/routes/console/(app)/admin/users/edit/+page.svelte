<script lang="ts">
  import { getInitials } from "$lib/control-panel.ts";
  import { ulid } from "ulid";
  import { apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { beforeNavigate } from "$app/navigation";
  import { onDestroy, untrack } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import UserSecurity from "$lib/components/UserSecurity.svelte";
  import UserAccess from "$lib/components/UserAccess.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-edit");
  const mutation = new MutationController<
    apis.auth.UsersUpdateInput,
    apis.auth.UsersUpdateOutput
  >();
  const requestedUserId = $derived(page.url.searchParams.get("userId") ?? "");
  let user = $state.raw<apis.auth.UsersGetOutput["user"] | null>(null);
  let loading = $state(true);
  let saving = $state(false);
  let displayName = $state("");
  let enabled = $state(true);
  let error = $state<string | null>(null);
  let saved = $state(false);
  const dirty = $derived(
    user !== null &&
      (displayName !== user.displayName ||
        enabled !== user.account?.principal.enabled),
  );

  beforeNavigate((navigation) => {
    if (
      dirty &&
      !saving &&
      !window.confirm("Leave without saving this user's profile changes?")
    )
      navigation.cancel();
  });

  async function load(userId: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    user = null;
    try {
      if (!userId) return;
      const input = apis.auth.API.actions["rpc:Users.Get"].input.decode({
        userId,
      });
      const response = await trellis.usersGet(input).orThrow();
      if (!scope.isCurrent(token)) return;
      if (response.user.principalId !== userId)
        throw new Error("User lookup returned a different identity.");
      if (!response.user.account)
        throw new Error("Account management authority is required.");
      user = response.user;
      displayName = user.displayName;
      enabled = response.user.account.principal.enabled;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function save(): Promise<void> {
    if (
      !user?.account ||
      loading ||
      saving ||
      error ||
      !canPerform(authority.authority, "usersUpdate")
    )
      return;
    const key = ulid();
    const input: apis.auth.UsersUpdateInput = {
      userId: user.principalId,
      ...(displayName.trim() !== user.displayName
        ? {
            displayName: displayName.trim(),
            expectedProfileRevision: user.account.profileRevision,
          }
        : {}),
      enabled,
      expectedRevision: user.account.principal.revision,
      idempotencyKey: key,
    };
    const intent = captureIntent({
      operation: "usersUpdate",
      targetId: input.userId,
      label: user.account.username,
      idempotencyKey: key,
      input,
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    saving = true;
    saved = false;
    const outcome = await mutation.send({
      isStillValid: () =>
        scope.key === intent.scope.routeKey &&
        user?.principalId === input.userId &&
        user.account?.principal.revision === input.expectedRevision,
      dispatch: async ({ input }) => await trellis.usersUpdate(input).orThrow(),
    });
    saving = false;
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") {
      user = outcome.value.user;
      displayName = user.displayName;
      enabled = user.account?.principal.enabled ?? enabled;
      saved = true;
    } else
      error =
        outcome.kind === "unknown"
          ? "Outcome unknown; reload this user before submitting another change."
          : projectConsoleError(outcome.error).message;
  }

  $effect(() => {
    const id = requestedUserId;
    scope.setKey(id);
    mutation.cancel();
    untrack(() => void load(id));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="max-w-4xl space-y-5">
  <PageToolbar eyebrow="Users" title={user?.account?.username ?? "Edit user"}>
    {#snippet actions()}<a
        class="btn btn-ghost btn-sm"
        href={resolve("/admin/users")}>Back to users</a
      >{/snippet}
  </PageToolbar>
  {#if error}<Notice variant="error"
      >{error}<button
        class="btn btn-ghost btn-xs ml-2"
        onclick={() => void load(requestedUserId)}>Reload user</button
      ></Notice
    >{/if}
  {#if saved}<Notice variant="success">Profile updated.</Notice>{/if}
  {#if loading}<LoadingState label="Loading user" />
  {:else if !user?.account}<EmptyState
      title="User not found"
      description="Go back to Users and choose someone from the list."
    />
  {:else}
    <form
      class="trellis-section divide-y divide-base-300"
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <section class="flex items-center gap-4 px-5 py-4">
        <span
          class="grid h-12 w-12 place-items-center rounded-full bg-primary text-base font-medium text-primary-content"
          aria-hidden="true">{getInitials(user)}</span
        >
        <div class="min-w-0 flex-1">
          <h2 class="truncate text-base font-semibold">{user.displayName}</h2>
          <p class="text-sm text-base-content/65">
            {user.account.username}{user.email ? ` · ${user.email}` : ""}
          </p>
        </div>
        <span class="flex items-center gap-1 text-xs text-base-content/55"
          >ID <CopyButton value={user.principalId} label="Copy user ID" /></span
        >
      </section>
      <label class="flex items-center justify-between gap-4 px-5 py-3">
        <span
          ><span class="block text-sm font-medium">Enabled</span><span
            class="trellis-field-help block"
            >Turn off to stop this person from signing in.</span
          ></span
        >
        <input
          class="toggle toggle-sm"
          type="checkbox"
          bind:checked={enabled}
          disabled={saving}
        />
      </label>
      <section class="grid gap-3 px-5 py-3 md:grid-cols-2">
        <label class="form-control"
          ><span class="trellis-field-label">Username</span><input
            class="input input-bordered input-sm mt-1"
            value={user.account.username}
            readonly
          /></label
        >
        <label class="form-control"
          ><span class="trellis-field-label">Display name</span><input
            class="input input-bordered input-sm mt-1"
            bind:value={displayName}
            required
            disabled={saving}
          /></label
        >
      </section>
      <div class="flex justify-end gap-2 px-5 py-3">
        <a class="btn btn-ghost" href={resolve("/admin/users")}>Cancel</a
        ><button
          class="btn btn-primary"
          type="submit"
          disabled={saving ||
            !!error ||
            !dirty ||
            !canPerform(authority.authority, "usersUpdate")}
          >{saving ? "Saving…" : "Save profile"}</button
        >
      </div>
    </form>
    <UserAccess principalId={user.principalId} />
    <UserSecurity
      userId={user.principalId}
      userLabel={user.account.username}
      disabled={saving || loading}
    />
  {/if}
</section>
