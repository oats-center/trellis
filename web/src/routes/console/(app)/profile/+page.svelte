<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { getInitials, getRoleLabel } from "$lib/control-panel.ts";
  import {
    formatIdentityProviderLabel,
    participantKindLabel,
    type ParticipantKind,
  } from "$lib/auth_display.ts";
  import { errorMessage, formatDate } from "$lib/format";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { getNotifications } from "$lib/notifications.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { getAuthenticatedUser, getConnection, getTrellis } from "$lib/trellis";

  type IdentityRecord = apis.auth.UserIdentitiesListOutput["items"][number];
  const trellis = getTrellis();
  const connection = getConnection();
  const notifications = getNotifications();

  let loading = $state(true);
  let error = $state<string | null>(null);
  let user = $state<apis.auth.SessionsMeOutput["user"] | null>(null);
  let participantKind = $state<ParticipantKind | null>(null);
  let platformPrivileges = $state<string[]>([]);
  let identities = $state.raw<IdentityRecord[]>([]);
  let identitiesPanelError = $state<string | null>(null);
  let linkPending = $state(false);
  let linkError = $state<string | null>(null);
  let linkUncertain = $state(false);
  let passwordChangePending = $state(false);
  let passwordChangeError = $state<string | null>(null);
  let passwordChangeUncertain = $state(false);
  let passwordChangeModalOpen = $state(false);
  let currentPassword = $state("");
  let newPassword = $state("");
  let confirmNewPassword = $state("");

  const connectionStatus = $derived(connection.status.phase);
  const hasLocalIdentity = $derived(identities.some((identity) => identity.providerId.trim().toLowerCase() === "local"));
  const capabilityCount = $derived(platformPrivileges.length);
  const accountRole = $derived(getRoleLabel({ platformPrivileges }));
  const sessionStatusLabel = $derived(
    connectionStatus === "connected" ? "Connected" : connectionStatus === "reconnecting" ? "Reconnecting" : "Disconnected",
  );
  const mastheadSentence = $derived(
    `This account signs in through ${identities.length} ${identities.length === 1 ? "method" : "methods"}.`,
  );

  function friendlyIdentityName(identity: IdentityRecord): string {
    return identity.observedName?.trim() || identity.observedEmail?.trim() || formatIdentityProviderLabel(identity.providerId);
  }

  function capabilityCountLabel(count: number): string {
    return `${count} ${count === 1 ? "privilege" : "privileges"}`;
  }

  function isLocalIdentity(identity: IdentityRecord): boolean {
    return identity.providerId.trim().toLowerCase() === "local";
  }

  function identityTitle(identity: IdentityRecord): string {
    if (isLocalIdentity(identity)) {
      return identity.subject.trim() || identity.observedName?.trim() || identity.observedEmail?.trim() || "Local account";
    }
    return friendlyIdentityName(identity);
  }

  function currentReturnTarget(): string {
    return `${page.url.pathname}${page.url.search}${page.url.hash}`;
  }

  function openPasswordModal(): void {
    passwordChangeError = null;
    currentPassword = "";
    newPassword = "";
    confirmNewPassword = "";
    passwordChangeModalOpen = true;
  }

  function closePasswordModal(): void {
    if (passwordChangePending) return;
    passwordChangeModalOpen = false;
    passwordChangeError = null;
    currentPassword = "";
    newPassword = "";
    confirmNewPassword = "";
  }

  async function createIdentityLink() {
    if (linkPending) return;
    linkPending = true;
    linkError = null;
    linkUncertain = false;
    try {
      const response = await trellis.usersIdentityLinkCreate({
        allowedProviders: [],
        idempotencyKey: ulid(),
        returnTarget: currentReturnTarget(),
      }).orThrow();
      window.location.assign(response.flow.completionUrl);
    } catch (e) {
      if (classifyMutationError(e).kind === "unknown") {
        linkUncertain = true;
        linkError = "The link flow outcome is unknown. Reload and check before starting another.";
        return;
      }
      linkError = errorMessage(e);
      notifications.error(linkError, "Connect login failed");
    } finally {
      linkPending = false;
    }
  }

  async function changePassword() {
    if (passwordChangePending) return;
    passwordChangePending = true;
    passwordChangeError = null;
    passwordChangeUncertain = false;
    try {
      if (!currentPassword || !newPassword) {
        passwordChangeError = "Enter your current password and a new password.";
        return;
      }
      if (newPassword !== confirmNewPassword) {
        passwordChangeError = "The new password fields do not match.";
        return;
      }

      await trellis.usersPasswordChange({
        currentPassword,
        idempotencyKey: ulid(),
        newPassword,
      }).orThrow();
      currentPassword = "";
      newPassword = "";
      confirmNewPassword = "";
      passwordChangeModalOpen = false;
      notifications.success("Password changed. Other sessions may need to sign in again.", "Password changed");
    } catch (e) {
      if (classifyMutationError(e).kind === "unknown") {
        passwordChangeUncertain = true;
        passwordChangeError = "The password change outcome is unknown. Other sessions may already be signed out. Try signing in with the new password before attempting another change.";
        return;
      }
      passwordChangeError = errorMessage(e);
      notifications.error(passwordChangeError, "Password change failed");
    } finally {
      passwordChangePending = false;
    }
  }

  async function loadProfile() {
    loading = true;
    error = null;
    try {
      const me = await getAuthenticatedUser(trellis);
      user = me.user ?? null;
      const kind = me.connection.participantKind;
      if (kind === "app") participantKind = "app";
      else if (kind === "agent") participantKind = "agent";
      else if (kind === "device") participantKind = "device";
      else if (kind === "service") participantKind = "service";
      else participantKind = null;
      platformPrivileges = me.connection.platformPrivileges;
      if (!me.user) {
        identities = [];
        return;
      }

      // The identity panel loads independently: a failure here must not erase
      // the account information that already loaded.
      identitiesPanelError = null;
      try {
        const identitiesResult = await traverseAll<IdentityRecord>(
          async (pageRequest) => {
            const response = await trellis.userIdentitiesList({
              page: catalogPage(pageRequest.cursor),
            }).take();
            if (isErr(response)) throw response;
            return { items: response.items, cursor: response.page.nextCursor };
          },
        );
        if (!identitiesResult.complete) {
          identitiesPanelError = errorMessage(identitiesResult.error);
          return;
        }
        identities = [...identitiesResult.items];
      } catch (cause) {
        identitiesPanelError = errorMessage(cause);
      }
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void loadProfile();
  });
</script>

{#if loading}
  <Panel><LoadingState label="Loading account" /></Panel>
{:else if error}
  <Notice variant="error" class="mb-4">{error}</Notice>
{:else if user}
  <section class="space-y-4">
    <PageToolbar title="Account Access Ledger" description="Manage how you sign in, which apps or agents can act for you, and what this account can do.">
      {#snippet actions()}
        <button class="btn btn-ghost btn-sm" onclick={loadProfile}>Refresh</button>
      {/snippet}
    </PageToolbar>

    <div class="rounded-box border border-base-300 bg-base-100 p-4">
      <div class="flex flex-col gap-3 md:flex-row md:items-center md:justify-between">
        <div class="flex min-w-0 items-start gap-3">
          {#if user.image}
            <div class="avatar"><div class="w-12 rounded-full"><img src={user.image} alt={user.name} /></div></div>
          {:else}
            <div class="avatar avatar-placeholder"><div class="w-12 rounded-full bg-neutral text-neutral-content"><span class="text-base">{getInitials(user)}</span></div></div>
          {/if}
          <div class="min-w-0 space-y-1">
            <div class="flex flex-wrap items-center gap-2">
              <h2 class="truncate text-xl font-semibold">{user.name}</h2>
              <span class="badge badge-outline badge-sm">{sessionStatusLabel}</span>
            </div>
            {#if user.email}
              <p class="truncate text-sm text-base-content/70">{user.email}</p>
            {:else}
              <p class="text-sm text-base-content/70">No email address is saved on this account.</p>
            {/if}
            <p class="max-w-3xl text-sm text-base-content/70">{mastheadSentence}</p>
          </div>
        </div>
        <dl class="grid shrink-0 grid-cols-2 gap-x-6 gap-y-1 text-sm md:text-right">
          <div>
            <dt class="text-xs font-medium uppercase tracking-wide text-base-content/50">Role</dt>
            <dd class="font-medium">{accountRole}</dd>
          </div>
          <div>
            <dt class="text-xs font-medium uppercase tracking-wide text-base-content/50">Session</dt>
            <dd class="font-medium">{participantKind ? participantKindLabel(participantKind) : "Unknown"}</dd>
          </div>
        </dl>
      </div>
    </div>

    <Panel title="Ledger">
      <div class="divide-y divide-base-300">
        <section class="py-4 first:pt-0">
          <div class="grid gap-3 lg:grid-cols-[14rem_minmax(0,1fr)]">
            <div>
              <h3 class="font-semibold">How you sign in</h3>
              <p class="mt-1 text-sm text-base-content/60">Connect a provider without creating another account.</p>
            </div>
            <div class="space-y-3">
              <div class="flex flex-wrap items-center justify-between gap-2">
                <p class="text-sm text-base-content/70">Connected logins unlock the same Trellis account.</p>
                <button class="btn btn-outline btn-sm" type="button" onclick={createIdentityLink} disabled={linkPending}>{linkPending ? "Opening..." : "Connect another login"}</button>
              </div>
              {#if linkError}<Notice variant={linkUncertain ? "warning" : "error"} class="text-sm" role="alert">{linkError}</Notice>{/if}
              {#if identitiesPanelError}
                <Notice variant="warning" class="text-sm">
                  Identities could not be loaded: {identitiesPanelError}
                  <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={loadProfile}>Retry</button>
                </Notice>
              {/if}

              <div class="divide-y divide-base-300 rounded-box border border-base-300">
                {#each identities as identity (`${identity.providerId}:${identity.subject}`)}
                  <div class="p-3">
                    <div class="grid gap-3 md:grid-cols-[minmax(0,1.1fr)_minmax(0,2fr)] md:items-start">
                      <div class="min-w-0">
                        <div class="truncate font-medium">{identityTitle(identity)}</div>
                        <div class="text-xs text-base-content/60">{isLocalIdentity(identity) ? "Local password" : formatIdentityProviderLabel(identity.providerId)}</div>
                      </div>
                        <div class="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
                          <dl class="grid gap-x-4 gap-y-1 text-xs text-base-content/60 sm:grid-cols-3">
                            <div><dt class="font-semibold text-base-content/70">Email</dt><dd class="truncate">{identity.observedEmail || "Not provided"}</dd></div>
                            <div><dt class="font-semibold text-base-content/70">Linked</dt><dd>{formatDate(identity.createdAt)}</dd></div>
                            <div><dt class="font-semibold text-base-content/70">Last used</dt><dd>{formatDate(identity.lastSeenAt)}</dd></div>
                          </dl>
                          {#if isLocalIdentity(identity)}
                            <button class="btn btn-outline btn-xs" type="button" onclick={openPasswordModal}>Change password</button>
                          {/if}
                        </div>
                      </div>
                    </div>
                {:else}
                  <div class="p-3"><EmptyState title="No sign-in methods found" description="Refresh the page or contact an administrator if you cannot sign in again." /></div>
                {/each}
              </div>

              {#if !hasLocalIdentity}
                <p class="text-sm text-base-content/60">No local password is connected to this account. Your connected login providers still work.</p>
              {/if}
            </div>
          </div>
        </section>

        <section class="py-4 last:pb-0">
          <div class="grid gap-3 lg:grid-cols-[14rem_minmax(0,1fr)]">
            <div>
              <h3 class="font-semibold">What you can do</h3>
              <p class="mt-1 text-sm text-base-content/60">Direct privileges assigned to this account.</p>
            </div>
            <div class="space-y-2">
              <div class="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
                <div>
                  <p class="font-medium">{accountRole}</p>
                  <p class="text-sm text-base-content/60">This account has {capabilityCount} direct {capabilityCount === 1 ? "privilege" : "privileges"}.</p>
                </div>
                <span class="badge badge-outline badge-sm">{capabilityCountLabel(capabilityCount)}</span>
              </div>
            </div>
          </div>
        </section>
      </div>
    </Panel>
  </section>
{/if}

{#if passwordChangeModalOpen}
  <dialog class="modal modal-open" open aria-labelledby="password-change-title" onclick={(event) => { if (event.target === event.currentTarget) closePasswordModal(); }}>
    <div class="modal-box relative w-full max-w-md border border-base-300 p-0">
      <div class="flex items-start justify-between gap-3 border-b border-base-300 p-4">
        <div>
          <h3 id="password-change-title" class="font-semibold">Change local password</h3>
          <p class="mt-1 text-sm text-base-content/60">Use your current password to update this account.</p>
        </div>
        <button class="btn btn-ghost btn-sm btn-square" type="button" aria-label="Cancel password change" onclick={closePasswordModal} disabled={passwordChangePending}>✕</button>
      </div>
      <form class="grid gap-3 p-4" onsubmit={(event) => { event.preventDefault(); void changePassword(); }}>
        <label class="form-control w-full">
          <span class="label py-1"><span class="label-text text-xs">Current password</span></span>
          <input class="input input-bordered input-sm" type="password" bind:value={currentPassword} autocomplete="current-password" required />
        </label>
        <label class="form-control w-full">
          <span class="label py-1"><span class="label-text text-xs">New password</span></span>
          <input class="input input-bordered input-sm" type="password" bind:value={newPassword} autocomplete="new-password" required />
        </label>
        <label class="form-control w-full">
          <span class="label py-1"><span class="label-text text-xs">Confirm new password</span></span>
          <input class="input input-bordered input-sm" type="password" bind:value={confirmNewPassword} autocomplete="new-password" required />
        </label>
        {#if passwordChangeError}<Notice variant={passwordChangeUncertain ? "warning" : "error"} class="text-sm" role="alert">{passwordChangeError}</Notice>{/if}
        <div class="flex justify-end gap-2 pt-1">
          <button class="btn btn-ghost btn-sm" type="button" onclick={closePasswordModal} disabled={passwordChangePending}>Cancel</button>
          <button class="btn btn-primary btn-sm" type="submit" disabled={passwordChangePending}>{passwordChangePending ? "Changing..." : "Change password"}</button>
        </div>
      </form>
    </div>
  </dialog>
{/if}
