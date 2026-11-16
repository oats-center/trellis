<script lang="ts">
  import { onMount, untrack } from "svelte";
  import type { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { isUncertainError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import GroupRoles from "./GroupRoles.svelte";

  type Settings = apis.auth.SignInSettingsGetOutput["settings"];
  type Provider = apis.auth.OIDCProvidersListOutput["items"][number];

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("sign-in-methods");
  let settings = $state.raw<Settings | null>(null);
  let localUsers = $state(true);
  let localSignUp = $state(false);
  let oidcSignIn = $state(true);
  let providers = $state.raw<Provider[]>([]);
  let loading = $state(true);
  let loaded = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
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
  const activeProviders = $derived(
    providers.filter((provider) => provider.enabled),
  );
  // Each section saves its own switches with the other section's saved
  // values, so nobody can sign in after a save when both end up off. The
  // server refuses this too; only providers this person can see count.
  const noActiveProvider = $derived(
    manageProviders && activeProviders.length === 0,
  );
  const localLockedOut = $derived(
    settings !== null &&
      !localUsers &&
      (!settings.providers || noActiveProvider),
  );
  const oidcLockedOut = $derived(
    settings !== null &&
      !settings.localUsers &&
      (!oidcSignIn || noActiveProvider),
  );
  const localChanged = $derived(
    settings !== null &&
      (settings.localUsers !== localUsers ||
        settings.localSignUp !== localSignUp),
  );
  const oidcChanged = $derived(
    settings !== null && settings.providers !== oidcSignIn,
  );
  // On with nothing to sign in through: nobody sees the option yet.
  const oidcIdle = $derived(
    oidcSignIn && manageProviders && activeProviders.length === 0,
  );
  // Names the providers people see, when this person can list them.
  const oidcSummary = $derived.by(() => {
    if (!oidcSignIn) return "Nobody can sign in with an OIDC account.";
    if (!manageProviders)
      return "Users sign in with their organisation's account.";
    if (activeProviders.length === 0)
      return "Users sign in through a provider.";
    if (activeProviders.length > 2)
      return `Users sign in through ${activeProviders.length} providers.`;
    return `Users sign in through ${activeProviders.map((provider) => provider.title).join(" or ")}.`;
  });

  // Shows the saved settings, keeping a section's unsaved edits unless that
  // section was the one just saved.
  function show(next: Settings, saved?: "local" | "oidc"): void {
    const keepLocal = localChanged && saved !== "local";
    const keepOidc = oidcChanged && saved !== "oidc";
    settings = next;
    if (!keepLocal) {
      localUsers = next.localUsers;
      localSignUp = next.localSignUp;
    }
    if (!keepOidc) oidcSignIn = next.providers;
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
      if (current) show(current.settings);
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

  // One update holds both sections; the other section keeps its saved values.
  function saveSettings(section: "local" | "oidc"): Promise<void> {
    return run(async () => {
      if (!settings) return null;
      const local = section === "local";
      const users = local ? localUsers : settings.localUsers;
      const response = await trellis
        .signInSettingsUpdate({
          localUsers: users,
          localSignUp: users && (local ? localSignUp : settings.localSignUp),
          providers: local ? settings.providers : oidcSignIn,
          expectedRevision: settings.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      show(response.settings, section);
      return local ? "Local users saved." : "OIDC sign-in saved.";
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
          idempotencyKey: ulid(),
        })
        .orThrow();
      editing = false;
      await load();
      return "Provider saved.";
    });
  }

  function change(operation: "disable" | "delete"): Promise<void> {
    return run(async () => {
      if (
        !selected ||
        !window.confirm(
          `${operation === "disable" ? "Turn off" : "Delete"} ${selected.title}?`,
        )
      )
        return null;
      const target = {
        providerId: selected.providerId,
        expectedRevision: selected.revision,
        idempotencyKey: ulid(),
      };
      if (operation === "disable")
        await trellis.oidcProvidersDisable(target).orThrow();
      else await trellis.oidcProvidersDelete(target).orThrow();
      editing = false;
      await load();
      return operation === "disable"
        ? "Provider turned off."
        : "Provider deleted.";
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

<section class="space-y-6">
  <PageToolbar
    title="Sign-in methods"
    description="Choose how users sign in to this Trellis. Each section applies when you save it."
  />
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}

  {#if loading && !loaded}
    <LoadingState />
  {:else}
    {#if settings}
      <section class="space-y-3" aria-labelledby="section-local">
        <h2 class="text-lg font-semibold" id="section-local">Local users</h2>
        <form
          class="max-w-2xl space-y-3"
          onsubmit={(event) => {
            event.preventDefault();
            void saveSettings("local");
          }}
        >
          <fieldset class="space-y-2" disabled={saving || !editableSettings}>
            <div class="method" class:on={localUsers}>
              <label class="method-row"
                ><span class="method-mark"><Icon name="key" size={18} /></span
                ><span class="min-w-0 flex-1"
                  ><span class="method-name" id="method-local"
                    >Username and password</span
                  ><span class="method-state" id="method-local-state"
                    ><span class="method-word">{localUsers ? "On" : "Off"}</span
                    >
                    · {localUsers
                      ? "Users with an account here sign in with their password."
                      : "Nobody can sign in with a password."}{#if settings.localUsers !== localUsers}<span
                        class="method-unsaved">Not saved</span
                      >{/if}</span
                  ></span
                ><input
                  class="toggle toggle-primary"
                  type="checkbox"
                  role="switch"
                  aria-labelledby="method-local"
                  aria-describedby="method-local-state"
                  bind:checked={localUsers}
                /></label
              >
              <label class="method-option" class:muted={!localUsers}
                ><input
                  class="checkbox checkbox-sm"
                  type="checkbox"
                  bind:checked={localSignUp}
                  disabled={!localUsers}
                /><span
                  >Users can create their own account <span
                    class="text-base-content/60"
                    >· in apps that also allow it</span
                  >{#if settings.localSignUp !== localSignUp}<span
                      class="method-unsaved">Not saved</span
                    >{/if}</span
                ></label
              >
            </div>
            {#if localLockedOut}
              <p class="text-sm text-error" role="alert">
                {settings.providers
                  ? "Add an active OIDC provider first, or nobody can sign in."
                  : "Turn on OIDC sign-in and save it first, or nobody can sign in."}
              </p>
            {/if}
          </fieldset>
          {#if editableSettings}
            <div class="flex justify-end">
              <button
                class="btn btn-primary"
                disabled={saving || !localChanged || localLockedOut}
                >Save</button
              >
            </div>
          {/if}
        </form>
      </section>
    {/if}

    {#if settings || manageProviders}
      <section class="space-y-3" aria-labelledby="section-oidc">
        <h2 class="text-lg font-semibold" id="section-oidc">OIDC sign-in</h2>
        {#if settings}<form
            class="max-w-2xl space-y-3"
            onsubmit={(event) => {
              event.preventDefault();
              void saveSettings("oidc");
            }}
          >
            <fieldset class="space-y-2" disabled={saving || !editableSettings}>
              <div class="method" class:on={oidcSignIn}>
                <label class="method-row"
                  ><span class="method-mark"
                    ><Icon name="globe" size={18} /></span
                  ><span class="min-w-0 flex-1"
                    ><span class="method-name" id="method-oidc"
                      >Sign in through a provider</span
                    ><span class="method-state" id="method-oidc-state"
                      ><span class="method-word"
                        >{oidcSignIn ? "On" : "Off"}</span
                      >
                      · {oidcSummary}{#if settings.providers !== oidcSignIn}<span
                          class="method-unsaved">Not saved</span
                        >{/if}</span
                    ></span
                  ><input
                    class="toggle toggle-primary"
                    type="checkbox"
                    role="switch"
                    aria-labelledby="method-oidc"
                    aria-describedby={oidcIdle
                      ? "method-oidc-state method-oidc-idle"
                      : "method-oidc-state"}
                    bind:checked={oidcSignIn}
                  /></label
                >
                {#if oidcIdle}
                  <p class="method-warning" id="method-oidc-idle">
                    <Icon name="alert" size={16} /><span
                      >No provider is turned on, so nobody sees this option yet.</span
                    >
                  </p>
                {/if}
              </div>
              {#if oidcLockedOut}
                <p class="text-sm text-error" role="alert">
                  {oidcSignIn
                    ? "Add an active OIDC provider first, or nobody can sign in."
                    : "Turn on username and password and save it first, or nobody can sign in."}
                </p>
              {/if}
            </fieldset>
            {#if editableSettings}
              <div class="flex justify-end">
                <button
                  class="btn btn-primary"
                  disabled={saving || !oidcChanged || oidcLockedOut}
                  >Save</button
                >
              </div>
            {/if}
          </form>{/if}

        {#if manageProviders}
          <div class="space-y-3 pt-2">
            <div class="flex items-center justify-between">
              <h3 class="font-semibold">Providers</h3>
              {#if canPerform(authority.authority, "oidcProvidersPut")}<button
                  type="button"
                  class="btn btn-sm"
                  disabled={saving}
                  onclick={() => edit(null)}
                  ><Icon name="plus" size={16} /> Add provider</button
                >{/if}
            </div>
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
                    ><th><span class="max-sm:sr-only">Status</span></th><th
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
                          class="block font-mono text-xs text-base-content/60 sm:hidden"
                          >{provider.issuer}</span
                        >
                      </td>
                      <td class="hidden font-mono text-sm sm:table-cell"
                        >{provider.issuer}</td
                      >
                      <td
                        ><StatusBadge
                          compact
                          label={provider.enabled ? "On" : "Off"}
                          status={provider.enabled ? "healthy" : "offline"}
                        /></td
                      >
                      <td class="text-right"
                        ><button
                          type="button"
                          class="btn btn-ghost btn-sm"
                          onclick={() => edit(provider)}>Edit</button
                        ></td
                      >
                    </tr>
                  {/each}
                </tbody>
              </DataTable>
            {/if}
          </div>
        {/if}
        {#if editing}
          <form
            class="trellis-panel max-w-2xl space-y-4 p-5"
            onsubmit={saveProvider}
          >
            <div class="flex items-center justify-between">
              <h2 class="text-lg font-semibold">
                {selected ? selected.title : "New provider"}
              </h2>
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
            <div
              class="flex flex-wrap justify-between gap-2 border-t border-base-300 pt-4"
            >
              <div class="flex gap-2">
                {#if selected && canPerform(authority.authority, "oidcProvidersDisable")}<button
                    type="button"
                    class="btn btn-outline"
                    disabled={saving || !selected.enabled}
                    onclick={() => void change("disable")}>Turn off</button
                  >{/if}
                {#if selected && canPerform(authority.authority, "oidcProvidersDelete")}<button
                    type="button"
                    class="btn btn-outline btn-error"
                    disabled={saving}
                    onclick={() => void change("delete")}>Delete</button
                  >{/if}
              </div>
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
</section>

<style>
  .method {
    border: 1px solid var(--color-base-300);
    border-radius: var(--radius-box);
    background: var(--color-base-100);
    transition:
      border-color 150ms ease-out,
      background-color 150ms ease-out;
  }
  .method.on {
    border-color: color-mix(in oklab, var(--color-primary) 45%, transparent);
    background: color-mix(
      in oklab,
      var(--color-primary) 5%,
      var(--color-base-100)
    );
  }
  .method:has(.toggle:focus-visible) {
    box-shadow: var(--trellis-focus-ring);
  }
  .method-row {
    align-items: center;
    cursor: pointer;
    display: flex;
    gap: 0.875rem;
    padding: 0.875rem 1rem;
  }
  fieldset:disabled .method-row {
    cursor: default;
  }
  .method-mark {
    align-items: center;
    background: var(--color-base-200);
    border-radius: var(--radius-field);
    color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
    display: flex;
    flex: none;
    height: 2.25rem;
    justify-content: center;
    transition:
      background-color 150ms ease-out,
      color 150ms ease-out;
    width: 2.25rem;
  }
  .on .method-mark {
    background: color-mix(in oklab, var(--color-primary) 16%, transparent);
    color: var(--color-primary);
  }
  .method-name {
    display: block;
    font-weight: 600;
  }
  .method-state {
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    display: block;
    font-size: 0.8125rem;
  }
  .method-word {
    font-weight: 600;
  }
  .on .method-word {
    color: var(--color-success);
  }
  .method-warning {
    align-items: center;
    border-top: 1px solid
      color-mix(in oklab, var(--color-warning) 35%, transparent);
    /* Warning alone is too light to read at this size on a pale panel. */
    color: color-mix(
      in oklab,
      var(--color-warning) 70%,
      var(--color-base-content)
    );
    display: flex;
    font-size: 0.875rem;
    font-weight: 500;
    gap: 0.5rem;
    /* Lines the text up under the method's name. */
    padding: 0.625rem 1rem 0.625rem calc(1rem + 2.25rem + 0.875rem);
  }
  .method-warning :global(svg) {
    flex: none;
    margin-left: calc(-16px - 0.5rem);
  }
  .method-unsaved {
    background: color-mix(in oklab, var(--color-warning) 16%, transparent);
    border-radius: var(--radius-selector);
    color: var(--color-base-content);
    font-size: 0.75rem;
    font-weight: 500;
    margin-left: 0.5rem;
    padding: 0.0625rem 0.375rem;
    white-space: nowrap;
  }
  .method-option {
    align-items: center;
    border-top: 1px solid var(--color-base-300);
    cursor: pointer;
    display: flex;
    font-size: 0.875rem;
    gap: 0.75rem;
    /* Lines the checkbox up under the method's name. */
    padding: 0.75rem 1rem 0.75rem calc(1rem + 2.25rem + 0.875rem);
  }
  .method.on .method-option {
    border-top-color: color-mix(
      in oklab,
      var(--color-primary) 25%,
      transparent
    );
  }
  .method-option.muted {
    color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
    cursor: default;
  }
  @media (max-width: 30rem) {
    .method-option {
      padding-left: 1rem;
    }
    .method-warning {
      padding-left: 1rem;
    }
    .method-warning :global(svg) {
      margin-left: 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .method,
    .method-mark {
      transition: none;
    }
  }
</style>
