<script lang="ts">
  import { onMount } from "svelte";
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

  type Settings = apis.auth.SignInSettingsGetOutput["settings"];
  type Provider = apis.auth.OIDCProvidersListOutput["items"][number];

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("sign-in-methods");
  let settings = $state.raw<Settings | null>(null);
  let localUsers = $state(true);
  let localSignUp = $state(false);
  let companySignIn = $state(true);
  let providers = $state.raw<Provider[]>([]);
  let loading = $state(true);
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
  let scopes = $state("");
  let groupsClaim = $state("");

  const editableSettings = $derived(
    canPerform(authority.authority, "signInSettingsUpdate"),
  );
  const manageProviders = $derived(
    canPerform(authority.authority, "oidcProvidersList"),
  );
  const changed = $derived(
    settings !== null &&
      (settings.localUsers !== localUsers ||
        settings.localSignUp !== localSignUp ||
        settings.providers !== companySignIn),
  );

  function show(next: Settings): void {
    settings = next;
    localUsers = next.localUsers;
    localSignUp = next.localSignUp;
    companySignIn = next.providers;
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    try {
      const [current, listed] = await Promise.all([
        trellis.signInSettingsGet({}).orThrow(),
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
      show(current.settings);
      providers = [...listed.items];
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

  function saveSettings(): Promise<void> {
    return run(async () => {
      if (!settings) return null;
      const response = await trellis
        .signInSettingsUpdate({
          localUsers,
          localSignUp: localUsers && localSignUp,
          providers: companySignIn,
          expectedRevision: settings.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      show(response.settings);
      return "Sign-in methods saved.";
    });
  }

  function edit(provider: Provider | null): void {
    selected = provider;
    title = provider?.title ?? "";
    issuer = provider?.issuer ?? "";
    clientId = provider?.clientId ?? "";
    clientSecret = "";
    scopes = (provider?.scopes ?? ["openid", "profile", "email"]).join(" ");
    groupsClaim = provider?.roleClaim ?? "";
    editing = true;
    error = null;
    notice = null;
  }

  function saveProvider(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    return run(async () => {
      await trellis
        .oidcProvidersPut({
          ...(selected ? { providerId: selected.providerId } : {}),
          title: title.trim(),
          issuer: issuer.trim(),
          clientId: clientId.trim(),
          // Left empty, an existing provider keeps its secret.
          ...(clientSecret ? { clientSecret } : {}),
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

  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="space-y-6">
  <PageToolbar
    title="Sign-in methods"
    description="Choose how people sign in to this Trellis. Changes apply at once."
  />
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}

  {#if loading && !settings}
    <LoadingState />
  {:else if settings}
    <form
      class="trellis-panel max-w-2xl space-y-4 p-5"
      onsubmit={(event) => {
        event.preventDefault();
        void saveSettings();
      }}
    >
      <fieldset class="space-y-3" disabled={saving || !editableSettings}>
        <label class="flex items-center gap-3"
          ><input
            class="toggle toggle-sm toggle-primary"
            type="checkbox"
            bind:checked={localUsers}
          /><span>Username and password</span></label
        >
        <label class="flex items-center gap-3 pl-8"
          ><input
            class="checkbox checkbox-sm"
            type="checkbox"
            bind:checked={localSignUp}
            disabled={!localUsers}
          /><span
            >People can create their own account <span
              class="text-sm text-base-content/60"
              >· in apps that also allow it</span
            ></span
          ></label
        >
        <label class="flex items-center gap-3"
          ><input
            class="toggle toggle-sm toggle-primary"
            type="checkbox"
            bind:checked={companySignIn}
          /><span>Company sign-in</span></label
        >
        {#if !localUsers && !companySignIn}
          <p class="text-sm text-error" role="alert">
            Keep at least one way to sign in, or nobody can.
          </p>
        {/if}
      </fieldset>
      {#if editableSettings}
        <div class="flex justify-end border-t border-base-300 pt-4">
          <button
            class="btn btn-primary"
            disabled={saving || !changed || (!localUsers && !companySignIn)}
            >Save</button
          >
        </div>
      {/if}
    </form>

    {#if manageProviders}
      <div class="space-y-3">
        <div class="flex items-center justify-between">
          <h2 class="text-lg font-semibold">Company sign-in providers</h2>
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
            description="Add your organisation's identity provider so people can sign in with it."
          />
        {:else}
          <DataTable>
            <thead
              ><tr><th>Name</th><th>Address</th><th>Status</th><th></th></tr
              ></thead
            >
            <tbody>
              {#each providers as provider (provider.providerId)}
                <tr>
                  <td class="font-medium">{provider.title}</td>
                  <td class="font-mono text-sm">{provider.issuer}</td>
                  <td>{provider.enabled ? "On" : "Off"}</td>
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
            />{#if selected?.hasClientSecret}<span class="trellis-field-help"
                >Leave empty to keep the current secret.</span
              >{/if}</label
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
                  >Where the provider lists a person's groups, for giving roles.</span
                ></label
              >
            </div>
          </details>
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
  {/if}
</section>
