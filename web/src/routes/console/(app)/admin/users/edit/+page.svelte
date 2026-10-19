<script lang="ts">
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
      displayName: displayName.trim(),
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

<section class="space-y-4">
  <PageToolbar
    title={user?.account?.username ?? "Edit user"}
    description="Manage the user's display name, sign-in availability, and authorization sessions."
  >
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
      title="User unavailable"
      description="Choose an existing user from the Users table. A failed lookup cannot be edited."
    />
  {:else}
    <form
      class="divide-y divide-base-300 border-y border-base-300 bg-base-100"
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <section class="px-5 py-3">
        <h2 class="text-base font-bold">{user.account.username}</h2>
        <p class="trellis-identifier mt-1">
          {user.principalId}<CopyButton
            value={user.principalId}
            label="Copy user ID"
          />
        </p>
        <p class="trellis-metadata mt-1">
          Revision {user.account.principal.revision}
        </p>
      </section>
      <label class="flex items-center justify-between gap-4 px-5 py-3">
        <span
          ><span class="block text-sm font-medium">Enabled</span><span
            class="trellis-field-help block"
            >Controls whether this user can authenticate.</span
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
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Cancel</a
        ><button
          class="btn btn-outline btn-sm"
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
