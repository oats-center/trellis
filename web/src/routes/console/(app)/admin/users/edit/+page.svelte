<script lang="ts">
  import { getInitials } from "$lib/control-panel.ts";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { apis } from "$lib/trellis-apis";
  import { page } from "$app/state";
  import { beforeNavigate, goto } from "$app/navigation";
  import { onDestroy, untrack } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import {
    captureIntent,
    classifyMutationError,
    MutationController,
  } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import {
    draftConflicts,
    rebaseDraft,
    settleConflict,
  } from "$lib/console/draft.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import DraftConflicts from "$lib/components/DraftConflicts.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import UserSecurity from "$lib/components/UserSecurity.svelte";
  import UserAccess from "$lib/components/UserAccess.svelte";
  import UserIdentities from "$lib/components/UserIdentities.svelte";
  import { getTrellis } from "$lib/trellis";

  type User = apis.auth.UsersGetOutput["user"];
  type Profile = { displayName: string; enabled: boolean };
  const profileOf = (user: User): Profile => ({
    displayName: user.displayName,
    enabled: user.account?.principal.enabled ?? false,
  });

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-edit");
  const mutation = new MutationController<
    apis.auth.UsersUpdateInput,
    apis.auth.UsersUpdateOutput
  >();
  const requestedUserId = $derived(page.url.searchParams.get("userId") ?? "");
  let user = $state.raw<User | null>(null);
  let loading = $state(true);
  let saving = $state(false);
  let displayName = $state("");
  let enabled = $state(true);
  // The saved profile each edit above was typed over. It differs from the
  // saved profile only for a field someone else changed meanwhile.
  let base = $state.raw<Profile>({ displayName: "", enabled: true });
  // A failed read, or a change whose outcome is unknown; both block saving.
  let error = $state<string | null>(null);
  // Why Auth refused the last change. The page rereads and stays usable.
  let refusal = $state<string | null>(null);
  let saved = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();
  const conflicts = $derived(
    user
      ? draftConflicts(
          { draft: { displayName, enabled }, base },
          profileOf(user),
        )
      : [],
  );
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

  // Takes a newer read of the user. What was typed stays, fields left alone
  // show what is saved now, and a field saved meanwhile becomes a conflict.
  function take(next: User): void {
    const merged = rebaseDraft(
      { draft: { displayName, enabled }, base },
      profileOf(next),
    );
    displayName = merged.draft.displayName;
    enabled = merged.draft.enabled;
    base = merged.base;
    user = next;
  }

  function settle(field: string, choice: "keep" | "take"): void {
    if (!user || (field !== "displayName" && field !== "enabled")) return;
    const settled = settleConflict(
      { draft: { displayName, enabled }, base },
      profileOf(user),
      field,
      choice,
    );
    displayName = settled.draft.displayName;
    enabled = settled.draft.enabled;
    base = settled.base;
  }

  async function load(userId: string): Promise<void> {
    const token = scope.begin();
    // A reread of the shown user keeps the form and what was typed in it.
    const reread = user?.principalId === userId;
    if (!reread) {
      loading = true;
      user = null;
    }
    error = null;
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
      if (reread && user?.principalId === userId) take(response.user);
      else {
        user = response.user;
        ({ displayName, enabled } = profileOf(response.user));
        base = profileOf(response.user);
      }
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
      conflicts.length > 0 ||
      !canPerform(authority.authority, "usersUpdate")
    )
      return;
    const key = codecs.ulid.decode(ulid());
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
    refusal = null;
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
      ({ displayName, enabled } = profileOf(user));
      base = profileOf(user);
      saved = true;
    } else if (outcome.kind === "unknown")
      error =
        "Outcome unknown; reload this user before submitting another change.";
    else {
      // A refused save usually means someone changed the account meanwhile;
      // reread it so the form shows what they saved beside what was typed.
      const message = projectConsoleError(outcome.error).message;
      await load(input.userId);
      if (scope.key === intent.scope.routeKey) refusal = message;
    }
  }

  async function remove(): Promise<void> {
    if (
      !user?.account ||
      saving ||
      !canPerform(authority.authority, "usersDelete")
    )
      return;
    const target = user;
    const username = user.account.username;
    const confirmed = await confirmationModal?.confirm({
      title: "Delete this account?",
      message:
        "They are signed out everywhere, and their sign-ins, profile and access are removed. This can't be undone.",
      confirmLabel: "Delete account",
      targetLabel: "Username",
      targetName: username,
      expectedValue: username,
    });
    if (!confirmed || user !== target || !target.account) return;
    const key = scope.key;
    saving = true;
    refusal = null;
    saved = false;
    try {
      await trellis
        .usersDelete({
          userId: target.principalId,
          expectedRevision: target.account.principal.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (scope.key === key) await goto(resolve("/admin/users"));
    } catch (cause) {
      if (scope.key !== key) return;
      const message = projectConsoleError(cause).message;
      // A refused delete means the account changed since it was read; show it
      // as it is now, keeping what was typed.
      if (classifyMutationError(cause).kind === "unknown") error = message;
      else {
        await load(target.principalId);
        if (scope.key === key) refusal = message;
      }
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    const id = requestedUserId;
    scope.setKey(id);
    mutation.cancel();
    // Untracked: the modal's own state must not rerun this effect, or opening
    // the delete confirmation would cancel it and reread the user.
    untrack(() => {
      confirmationModal?.cancel();
      void load(id);
    });
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
  {#if refusal}<Notice variant="error">{refusal}</Notice>{/if}
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
      {#if conflicts.length > 0}<div class="px-5 py-3">
          <DraftConflicts
            conflicts={conflicts.map((field) =>
              field === "displayName"
                ? {
                    field,
                    label: "Display name",
                    mine: displayName,
                    saved: user?.displayName ?? "",
                  }
                : {
                    field,
                    label: "Enabled",
                    mine: enabled ? "On" : "Off",
                    saved: user?.account?.principal.enabled ? "On" : "Off",
                  },
            )}
            disabled={saving}
            onsettle={settle}
          />
        </div>{/if}
      <div class="flex justify-between gap-2 px-5 py-3">
        <div>
          {#if canPerform(authority.authority, "usersDelete")}<button
              class="btn btn-error btn-outline"
              type="button"
              disabled={saving}
              onclick={() => void remove()}>Delete account</button
            >{/if}
        </div>
        <div class="flex gap-2">
          <a class="btn btn-ghost" href={resolve("/admin/users")}>Cancel</a
          ><button
            class="btn btn-primary"
            type="submit"
            disabled={saving ||
              !!error ||
              !dirty ||
              conflicts.length > 0 ||
              !canPerform(authority.authority, "usersUpdate")}
            >{saving ? "Saving…" : "Save profile"}</button
          >
        </div>
      </div>
    </form>
    <UserAccess principalId={user.principalId} />
    {#if canPerform(authority.authority, "usersFind")}<UserIdentities
        {user}
        disabled={saving || loading}
        onchange={take}
      />{/if}
    <UserSecurity
      userId={user.principalId}
      userLabel={user.account.username}
      disabled={saving || loading}
    />
  {/if}
</section>
<ConfirmationModal bind:this={confirmationModal} />
