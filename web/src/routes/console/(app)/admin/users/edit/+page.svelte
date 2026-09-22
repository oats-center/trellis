<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import { onDestroy, untrack } from "svelte";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type UserView = apis.auth.UsersGetOutput["user"];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-edit");
  type UpdateInput = Parameters<typeof trellis.usersUpdate>[0];
  const mutation = new MutationController<UpdateInput, apis.auth.UsersUpdateOutput>((state) => savePending = state.busy);

  const requestedUserId = $derived(page.url.searchParams.get("userId") ?? "");

  let loading = $state(true);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  /** True when the requested user is genuinely absent. */
  let notFound = $state(false);
  let targetUser = $state.raw<UserView | null>(null);
  let savePending = $state(false);

  // Editable fields are a copy of the loaded record, never a live view of it.
  let name = $state<string | null>(null);
  let email = $state<string | null>(null);
  let image = $state<string | null>(null);
  let active = $state(true);
  let revoked = $state(false);

  /** A revoked account is retained read-only and cannot be reactivated here. */
  const readOnly = $derived(revoked);

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  function applyUser(user: UserView): void {
    targetUser = user;
    name = user.name;
    email = user.email;
    image = user.image;
    active = user.state === "active";
    revoked = user.state === "revoked";
  }

  async function load(userId: string, preserveDraft = false): Promise<void> {
    const token = scope.begin();
    const keepDraft = preserveDraft && targetUser?.userId === userId;
    loading = true;
    error = null;
    notFound = false;
    if (!keepDraft) targetUser = null;
    try {
      if (userId === "") return;
      // Exact target through Users.Get, never a first-page list scan.
      const response = await trellis.usersGet({ userId }).take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = failureFrom(response);
        notFound = error.code === "not_found";
        targetUser = null;
        return;
      }
      if (response.user.userId !== userId) {
        error = { message: "User lookup returned a different identity." };
        return;
      }
      if (keepDraft) {
        targetUser = response.user;
        revoked = response.user.state === "revoked";
      } else {
        applyUser(response.user);
      }
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function saveUser(): Promise<void> {
    if (targetUser === null || readOnly || loading || savePending || error) return;
    if (targetUser.userId !== requestedUserId) return;
    const key = ulid();
    const intent = captureIntent<UpdateInput>({
      operation: "usersUpdate", targetId: targetUser.userId, label: targetUser.name ?? targetUser.userId,
      idempotencyKey: key,
      input: {
        userId: targetUser.userId,
        email: email?.trim() ? email.trim() : null,
        expectedVersion: targetUser.version,
        idempotencyKey: key,
        image: image?.trim() ? image.trim() : null,
        name: name?.trim() ? name.trim() : null,
        state: active ? "active" : "disabled",
      },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    error = null;
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && !loading && !readOnly &&

        targetUser?.userId === intent.targetId && targetUser.version === intent.input.expectedVersion && requestedUserId === intent.targetId,

      dispatch: async ({ input }) => await trellis.usersUpdate(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") {
      // Apply the returned record/version so a second save uses it.
      applyUser(outcome.value.user);
      notifications.success(`Updated ${outcome.value.user.name ?? outcome.value.user.userId}.`, "Updated");
    } else if (outcome.kind === "unknown") {
      error = { message: "Outcome unknown; the update may have completed. Reload this user before submitting another change." };
    } else {
      error = failureFrom(outcome.error);
    }
  }

  $effect(() => {
    const userId = requestedUserId;
    scope.setKey(JSON.stringify([userId]));
    mutation.cancel();
    untrack(() => void load(userId));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Edit user" description="Update a user's supported profile fields and activation state.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Back to users</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
      {#if !notFound}
        <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={() => void load(requestedUserId, targetUser !== null)}>{targetUser ? "Refresh version (keep draft)" : "Retry"}</button>
      {/if}
    </Notice>
  {/if}

  {#if loading}
    <div class="border-y border-base-300 bg-base-100 px-4 py-5">
      <LoadingState label="Loading user" />
    </div>
  {:else if error && targetUser === null && !notFound}
    <div class="border-y border-base-300 bg-base-100 px-4 py-5">User lookup did not complete. Retry before editing.</div>
  {:else if requestedUserId === ""}
    <EmptyState title="Choose a user" description="Open the Users table and choose Edit from the user's row actions.">
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/users")}>Back to users</a>
      {/snippet}
    </EmptyState>
  {:else if targetUser === null}
    <EmptyState
      title="User unavailable"
      description={`No user matches '${requestedUserId}'. It may have been removed, access may be denied, or the link may be stale.`}
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/users")}>Back to users</a>
        <button class="btn btn-ghost btn-sm" type="button" onclick={() => void load(requestedUserId)}>Retry</button>
      {/snippet}
    </EmptyState>
  {:else}
    <form class="divide-y divide-base-300 border-y border-base-300 bg-base-100" onsubmit={(event) => { event.preventDefault(); void saveUser(); }}>
      <section class="px-5 py-3">
        <p class="text-[0.65rem] font-semibold uppercase tracking-[0.12em] text-base-content/45">User</p>
        <div class="mt-1 flex min-w-0 flex-wrap items-end justify-between gap-3">
          <div class="min-w-0">
            <h2 class="truncate text-base font-bold leading-tight">{targetUser.name ?? targetUser.userId}</h2>
            <p class="trellis-metadata mt-1">{targetUser.email ?? "No email"}</p>
            <p class="trellis-identifier mt-1 break-all text-base-content/60">{targetUser.userId}</p>
          </div>
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Cancel</a>
        </div>
      </section>

      {#if revoked}
        <section class="px-5 py-3">
          <Notice variant="warning">
            This account is revoked. Its profile is retained read-only; a revoked account
            cannot be made active again from this form.
          </Notice>
        </section>
      {/if}

      <label class="flex items-center justify-between gap-4 px-5 py-3">
        <span class="min-w-0">
          <span class="block text-sm font-medium">Active</span>
          <span class="trellis-field-help block">Controls whether this user can authenticate.</span>
        </span>
        <input class="toggle toggle-sm" type="checkbox" bind:checked={active} disabled={savePending || readOnly} />
      </label>

      <section class="px-5 py-3">
        <div class="grid gap-3 md:grid-cols-2">
          <label class="form-control">
            <span class="trellis-field-label">Name</span>
            <input class="input input-bordered input-sm mt-1" bind:value={name} disabled={savePending || readOnly} />
          </label>
          <label class="form-control">
            <span class="trellis-field-label">Email</span>
            <input class="input input-bordered input-sm mt-1" type="email" bind:value={email} disabled={savePending || readOnly} />
          </label>
          <label class="form-control md:col-span-2">
            <span class="trellis-field-label">Image URL</span>
            <input class="input input-bordered input-sm mt-1 font-mono" bind:value={image} disabled={savePending || readOnly} />
          </label>
        </div>
        <dl class="mt-3 grid grid-cols-2 gap-3 text-xs text-base-content/60">
          <div>
            <dt class="uppercase tracking-wide">State</dt>
            <dd>
              <span class="badge badge-sm {targetUser.state === "active" ? "badge-success" : "badge-neutral"}">
                {targetUser.state}
              </span>
            </dd>
          </div>
          <div>
            <dt class="uppercase tracking-wide">Version</dt>
            <dd class="trellis-identifier">{targetUser.version}</dd>
          </div>
        </dl>
      </section>

      <section class="px-5 py-3">
        <p class="text-sm font-medium">Participant-owned grants</p>
        <p class="trellis-field-help mt-1">
          This account's effective participant authority is not a user-wide capability list.
          Inspect the exact owner and participant bindings in User grants.
        </p>
        <a
          class="btn btn-outline btn-sm mt-2"
          href={consoleUrl("/admin/apps", { query: { ownerKind: "user", ownerId: targetUser.userId } })}
        >Inspect user grants</a>
      </section>

      <div class="flex justify-end gap-2 px-5 py-3">
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/users")}>Cancel</a>
        <button class="btn btn-outline btn-sm" type="submit" disabled={savePending || readOnly || !!error}>
          {savePending ? "Saving…" : "Save user"}
        </button>
      </div>
    </form>
  {/if}
</section>
