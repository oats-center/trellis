<script lang="ts">
  import { onMount, untrack } from "svelte";
  import type { apis } from "$lib/trellis-apis";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { isConflictError, isUncertainError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import GroupRoles from "./GroupRoles.svelte";

  type Settings = apis.auth.SignInSettingsGetOutput["settings"];
  type Provider = apis.auth.OIDCProvidersListOutput["items"][number];

  const MIN_LENGTH = 8;
  const MAX_LENGTH = 128;

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("sign-in-methods");
  let settings = $state.raw<Settings | null>(null);
  let localUsers = $state(true);
  let localSignUp = $state(false);
  let passwordMinLength = $state<number | null>(8);
  let providers = $state.raw<Provider[]>([]);
  let loading = $state(true);
  let loaded = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  // Another administrator saved local-user settings after this draft began.
  let stale = $state(false);
  // The provider being edited; `null` with `editing` adds a new one.
  let editing = $state(false);
  let selected = $state.raw<Provider | null>(null);
  let title = $state("");
  let issuer = $state("");
  let clientId = $state("");
  let clientSecret = $state("");
  let certificateFingerprint = $state("");
  let scopes = $state("");
  let groupsClaim = $state("");

  const editableSettings = $derived(
    canPerform(authority.authority, "signInSettingsUpdate"),
  );
  const readSettings = $derived(
    canPerform(authority.authority, "signInSettingsGet"),
  );
  const manageProviders = $derived(
    canPerform(authority.authority, "oidcProvidersList"),
  );
  const switchProviders = $derived(
    canPerform(authority.authority, "oidcProvidersEnable") &&
      canPerform(authority.authority, "oidcProvidersDisable"),
  );
  const activeProviders = $derived(
    providers.filter((provider) => provider.enabled),
  );
  const lengthValid = $derived(
    passwordMinLength !== null &&
      Number.isInteger(passwordMinLength) &&
      passwordMinLength >= MIN_LENGTH &&
      passwordMinLength <= MAX_LENGTH,
  );
  // With username and password off, a provider that is on is the only way
  // in. The server refuses this too; only providers this person sees count.
  const lockedOut = $derived(
    !localUsers && manageProviders && activeProviders.length === 0,
  );
  const changed = $derived(
    settings !== null &&
      (settings.localUsers !== localUsers ||
        settings.localSignUp !== (localUsers && localSignUp) ||
        settings.passwordMinLength !== passwordMinLength),
  );
  const oidcStatus = $derived.by(() => {
    if (activeProviders.length === 0)
      return "No provider is on, so OIDC sign-in is off.";
    const through =
      activeProviders.length > 2
        ? `${activeProviders.length} providers`
        : activeProviders.map((provider) => provider.title).join(" or ");
    return `OIDC sign-in is on: users can sign in through ${through}.`;
  });

  function show(next: Settings): void {
    settings = next;
    localUsers = next.localUsers;
    localSignUp = next.localSignUp;
    passwordMinLength = next.passwordMinLength;
    stale = false;
  }

  // Drops the draft and shows the settings as they are saved now.
  function reloadSettings(): Promise<void> {
    if (settings) show(settings);
    return load();
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    try {
      const [current, listed] = await Promise.all([
        readSettings
          ? trellis.signInSettingsGet({}).orThrow()
          : Promise.resolve(null),
        manageProviders
          ? traverseAll(async (page) => {
              const response = await trellis
                .oidcProvidersList({ page })
                .orThrow();
              return {
                items: response.items,
                cursor: response.page.nextCursor,
              };
            })
          : Promise.resolve({ items: [] }),
      ]);
      if (!scope.isCurrent(token)) return;
      // Unsaved local-user changes survive a provider change, together with
      // the revision they were made from: saving them after another
      // administrator's change then conflicts instead of undoing it.
      if (current && !changed) show(current.settings);
      providers = [...listed.items];
      loaded = true;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function run(work: () => Promise<string | null>): Promise<void> {
    if (saving) return;
    error = null;
    notice = null;
    saving = true;
    try {
      notice = await work();
    } catch (cause) {
      error = isUncertainError(cause)
        ? "We couldn't confirm the change. Reload before trying again."
        : projectConsoleError(cause).message;
    } finally {
      saving = false;
    }
  }

  function saveSettings(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    return run(async () => {
      if (!settings || passwordMinLength === null) return null;
      let response;
      try {
        response = await trellis
          .signInSettingsUpdate({
            localUsers,
            localSignUp: localUsers && localSignUp,
            passwordMinLength,
            expectedRevision: settings.revision,
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow();
      } catch (cause) {
        if (!isConflictError(cause)) throw cause;
        stale = true;
        return null;
      }
      show(response.settings);
      return "Local users saved.";
    });
  }

  function turn(provider: Provider, on: boolean): Promise<void> {
    return run(async () => {
      const target = {
        providerId: provider.providerId,
        expectedRevision: provider.revision,
        idempotencyKey: codecs.ulid.decode(ulid()),
      };
      try {
        if (on) await trellis.oidcProvidersEnable(target).orThrow();
        else await trellis.oidcProvidersDisable(target).orThrow();
      } finally {
        // Shows the provider's saved state, also when the change was refused.
        await load();
      }
      return `${provider.title} turned ${on ? "on" : "off"}.`;
    });
  }

  function edit(provider: Provider | null): void {
    selected = provider;
    title = provider?.title ?? "";
    issuer = provider?.issuer ?? "";
    clientId = provider?.clientId ?? "";
    clientSecret = "";
    certificateFingerprint = provider?.certificateFingerprint ?? "";
    scopes = (provider?.scopes ?? ["openid", "profile", "email"]).join(" ");
    // Most providers list a person's groups in a `groups` claim.
    groupsClaim = provider ? provider.roleClaim : "/groups";
    editing = true;
    error = null;
    notice = null;
  }

  function saveProvider(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    const fingerprint = certificateFingerprint.trim();
    return run(async () => {
      await trellis
        .oidcProvidersPut({
          ...(selected ? { providerId: selected.providerId } : {}),
          title: title.trim(),
          issuer: issuer.trim(),
          clientId: clientId.trim(),
          // Left empty, an existing provider keeps its secret.
          ...(clientSecret ? { clientSecret } : {}),
          // Unchanged, the provider keeps its pin; cleared, `null` removes it.
          ...(fingerprint === (selected?.certificateFingerprint ?? "")
            ? {}
            : { certificateFingerprint: fingerprint || null }),
          scopes: scopes.split(/\s+/).filter(Boolean),
          roleClaim: groupsClaim.trim(),
          claimFreshnessSeconds: selected?.claimFreshnessSeconds ?? 3600,
          expectedRevision: selected?.revision ?? 0n,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      editing = false;
      await load();
      return "Provider saved.";
    });
  }

  function remove(provider: Provider): Promise<void> {
    return run(async () => {
      if (!window.confirm(`Delete ${provider.title}?`)) return null;
      await trellis
        .oidcProvidersDelete({
          providerId: provider.providerId,
          expectedRevision: provider.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (selected?.providerId === provider.providerId) editing = false;
      await load();
      return `${provider.title} deleted.`;
    });
  }

  // What to load depends on the signed-in person's permissions, which the
  // shell may still be verifying when this page mounts.
  let started = false;
  $effect(() => {
    if (started || authority.authority === null) return;
    started = true;
    untrack(() => void load());
  });
  onMount(() => () => scope.dispose());
</script>

<div class="max-w-3xl space-y-8">
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}

  {#if loading && !loaded}
    <LoadingState />
  {:else}
    {#if settings}
      <section aria-labelledby="section-local">
        <h2 class="section-title" id="section-local">Local users</h2>
        <form onsubmit={saveSettings}>
          <fieldset class="settings" disabled={saving || !editableSettings}>
            <label class="setting">
              <span class="setting-text">
                <span class="setting-label" id="local-users"
                  >Sign in with username and password</span
                >
                <span class="setting-help" id="local-users-help"
                  >Users with an account here sign in with their password.</span
                >
              </span>
              <input
                class="toggle toggle-primary"
                type="checkbox"
                role="switch"
                aria-labelledby="local-users"
                aria-describedby="local-users-help"
                bind:checked={localUsers}
              />
            </label>
            <label class="setting" class:muted={!localUsers}>
              <span class="setting-text">
                <span class="setting-label" id="local-sign-up"
                  >Users can create their own account</span
                >
                <span class="setting-help" id="local-sign-up-help"
                  >Only in apps that also allow it. Needs username and password
                  sign-in.</span
                >
              </span>
              <input
                class="toggle toggle-primary"
                type="checkbox"
                role="switch"
                aria-labelledby="local-sign-up"
                aria-describedby="local-sign-up-help"
                checked={localUsers && localSignUp}
                disabled={!localUsers}
                onchange={(event) =>
                  (localSignUp = event.currentTarget.checked)}
              />
            </label>
            <label class="setting">
              <span class="setting-text">
                <span class="setting-label" id="password-length"
                  >Minimum password length</span
                >
                <span class="setting-help" id="password-length-help"
                  >For new passwords, password changes and sign-up. From {MIN_LENGTH}
                  to {MAX_LENGTH} characters.</span
                >
              </span>
              <span class="flex items-center gap-2">
                <input
                  class="input input-sm w-20 text-right tabular-nums"
                  type="number"
                  inputmode="numeric"
                  min={MIN_LENGTH}
                  max={MAX_LENGTH}
                  step="1"
                  required
                  aria-labelledby="password-length"
                  aria-describedby="password-length-help"
                  aria-invalid={!lengthValid}
                  bind:value={passwordMinLength}
                /><span class="text-sm text-base-content/65" aria-hidden="true"
                  >characters</span
                >
              </span>
            </label>
          </fieldset>
          {#if stale}
            <div class="mt-3 flex flex-wrap items-center gap-3" role="alert">
              <p class="text-sm text-error">
                Another administrator changed these settings since you started.
                Nothing was saved. Reload to see their changes, then make yours
                again.
              </p>
              <button
                type="button"
                class="btn btn-sm"
                disabled={saving}
                onclick={() => void reloadSettings()}>Reload settings</button
              >
            </div>
          {/if}
          {#if lockedOut}
            <p class="mt-3 text-sm text-error" role="alert">
              Turn on an OIDC provider first, or nobody can sign in.
            </p>
          {/if}
          {#if editableSettings}
            <div class="mt-4 flex items-center justify-end gap-3">
              {#if changed}<span class="text-sm text-base-content/65"
                  >Unsaved changes</span
                >{/if}
              <button
                class="btn btn-primary"
                disabled={saving || !changed || lockedOut || !lengthValid}
                >Save</button
              >
            </div>
          {/if}
        </form>
      </section>
    {/if}

    {#if manageProviders}
      <section class="space-y-3" aria-labelledby="section-oidc">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <h2 class="section-title mb-0" id="section-oidc">OIDC providers</h2>
          {#if canPerform(authority.authority, "oidcProvidersPut")}<button
              type="button"
              class="btn btn-sm"
              disabled={saving}
              onclick={() => edit(null)}
              ><Icon name="plus" size={16} /> Add provider</button
            >{/if}
        </div>
        <p class="oidc-status" aria-live="polite">
          <span
            class={[
              "trellis-dot",
              activeProviders.length ? "success" : "neutral",
            ]}
            aria-hidden="true"
          ></span>{oidcStatus}
        </p>
        {#if providers.length === 0}
          <EmptyState
            title="No providers yet"
            description="Add your organisation's OIDC provider so users can sign in with it."
          />
        {:else}
          <DataTable>
            <thead
              ><tr
                ><th class="trellis-primary">Name</th><th
                  class="hidden sm:table-cell">Address</th
                ><th><span class="max-sm:sr-only">On</span></th><th
                  ><span class="sr-only">Actions</span></th
                ></tr
              ></thead
            >
            <tbody>
              {#each providers as provider (provider.providerId)}
                <tr>
                  <td class="trellis-primary">
                    <span class="block font-medium">{provider.title}</span>
                    <span
                      class="block font-mono text-xs break-all text-base-content/60 sm:hidden"
                      >{provider.issuer}</span
                    >
                  </td>
                  <td class="hidden font-mono text-sm sm:table-cell"
                    >{provider.issuer}</td
                  >
                  <td class="w-0">
                    <input
                      class="toggle toggle-primary toggle-sm"
                      type="checkbox"
                      role="switch"
                      aria-label={provider.title}
                      checked={provider.enabled}
                      disabled={saving || !switchProviders}
                      onchange={(event) =>
                        void turn(provider, event.currentTarget.checked)}
                    />
                  </td>
                  <td class="w-0 text-right">
                    <ActionMenu
                      widthClass="w-36"
                      ariaLabel={`Actions for ${provider.title}`}
                    >
                      <li>
                        <button
                          type="button"
                          disabled={saving}
                          onclick={() => edit(provider)}>Edit</button
                        >
                      </li>
                      {#if canPerform(authority.authority, "oidcProvidersDelete")}
                        <li>
                          <button
                            type="button"
                            class="text-error"
                            disabled={saving}
                            onclick={() => void remove(provider)}>Delete</button
                          >
                        </li>
                      {/if}
                    </ActionMenu>
                  </td>
                </tr>
              {/each}
            </tbody>
          </DataTable>
        {/if}
        {#if editing}
          <form class="trellis-panel space-y-4 p-5" onsubmit={saveProvider}>
            <div class="flex items-center justify-between">
              <h3 class="text-base font-semibold">
                {selected ? selected.title : "New provider"}
              </h3>
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-square"
                aria-label="Close"
                disabled={saving}
                onclick={() => (editing = false)}
                ><Icon name="close" size={16} /></button
              >
            </div>
            <fieldset class="space-y-4" disabled={saving}>
              <label class="trellis-field"
                ><span class="trellis-field-label">Name</span><input
                  class="input w-full"
                  bind:value={title}
                  required
                /><span class="trellis-field-help"
                  >Shown on the sign-in button.</span
                ></label
              >
              <label class="trellis-field"
                ><span class="trellis-field-label">Address</span><input
                  class="input w-full font-mono"
                  type="url"
                  bind:value={issuer}
                  placeholder="https://login.example.com"
                  required
                /></label
              >
              <label class="trellis-field"
                ><span class="trellis-field-label">Client ID</span><input
                  class="input w-full font-mono"
                  bind:value={clientId}
                  required
                /></label
              >
              <label class="trellis-field"
                ><span class="trellis-field-label">Client secret</span><input
                  class="input w-full font-mono"
                  type="password"
                  autocomplete="off"
                  bind:value={clientSecret}
                />{#if selected?.hasClientSecret}<span
                    class="trellis-field-help"
                    >Leave empty to keep the current secret.</span
                  >{/if}</label
              >
              <label class="trellis-field"
                ><span class="trellis-field-label">Certificate fingerprint</span
                ><input
                  class="input w-full font-mono"
                  autocomplete="off"
                  spellcheck="false"
                  bind:value={certificateFingerprint}
                /><span class="trellis-field-help"
                  >{selected?.certificateFingerprint
                    ? "This server trusts the provider's certificate issuer with this fingerprint. Clear it to trust only the system's certificates."
                    : "Only for a provider whose certificate this server doesn't trust. Confirm the fingerprint with the provider's operator."}</span
                ></label
              >
              <details>
                <summary class="cursor-pointer text-sm">More settings</summary>
                <div class="mt-3 space-y-4">
                  <label class="trellis-field"
                    ><span class="trellis-field-label">Scopes</span><input
                      class="input w-full font-mono"
                      bind:value={scopes}
                    /></label
                  >
                  <label class="trellis-field"
                    ><span class="trellis-field-label">Groups claim</span><input
                      class="input w-full font-mono"
                      bind:value={groupsClaim}
                      placeholder="/groups"
                    /><span class="trellis-field-help"
                      >Where the provider lists a person's groups, for giving
                      roles.</span
                    ></label
                  >
                </div>
              </details>
              {#if selected && canPerform(authority.authority, "oidcRoleMappingsList")}
                <!-- A new provider gets a fresh editor, never another's mappings. -->
                {#key selected.providerId}
                  <GroupRoles
                    providerId={selected.providerId}
                    claim={selected.roleClaim}
                  />
                {/key}
              {/if}
            </fieldset>
            <div class="flex justify-end gap-2 border-t border-base-300 pt-4">
              <button
                type="button"
                class="btn btn-ghost"
                disabled={saving}
                onclick={() => (editing = false)}>Cancel</button
              >
              {#if canPerform(authority.authority, "oidcProvidersPut")}<button
                  class="btn btn-primary"
                  disabled={saving}>Save provider</button
                >{/if}
            </div>
          </form>
        {/if}
      </section>
    {/if}
  {/if}
</div>

<style>
  .section-title {
    font-size: 1rem;
    font-weight: 600;
    margin-bottom: 0.25rem;
  }
  .section-title.mb-0 {
    margin-bottom: 0;
  }
  .settings {
    border-bottom: 1px solid var(--color-base-300);
  }
  .setting {
    align-items: center;
    border-top: 1px solid var(--color-base-300);
    cursor: pointer;
    display: flex;
    gap: 1rem;
    justify-content: space-between;
    padding: 0.875rem 0;
  }
  fieldset:disabled .setting,
  .setting.muted {
    cursor: default;
  }
  .setting-text {
    display: flex;
    flex-direction: column;
    gap: 0.125rem;
    min-width: 0;
  }
  .setting-label {
    font-size: 0.9375rem;
    font-weight: 500;
  }
  .setting-help {
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    font-size: 0.8125rem;
  }
  .setting.muted .setting-label {
    color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
  }
  .oidc-status {
    align-items: baseline;
    color: color-mix(in oklab, var(--color-base-content) 75%, transparent);
    display: flex;
    font-size: 0.875rem;
    gap: 0.5rem;
  }
  /* Sits on the first line's middle, also when the sentence wraps. */
  .oidc-status :global(.trellis-dot) {
    transform: translateY(-0.06em);
  }
</style>
