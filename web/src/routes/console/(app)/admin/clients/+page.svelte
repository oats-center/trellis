<script lang="ts">
  import { onMount } from "svelte";
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { resolve } from "$lib/console_paths";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { isUncertainError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Pager from "$lib/components/Pager.svelte";
  import PermissionGroups from "$lib/components/PermissionGroups.svelte";
  import { traverseAll } from "$lib/console/paging.ts";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";

  type Client = apis.auth.OAuthClientsGetOutput["client"];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("oauth-clients");
  let clients = $state.raw<Client[]>([]);
  let cursor = $state<string | undefined>();
  let next = $state<string | undefined>();
  let history = $state<(string | undefined)[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let editing = $state(false);
  let selected = $state.raw<Client | null>(null);
  let clientId = $state("");
  let title = $state("");
  let description = $state("");
  let kind = $state("browser");
  let development = $state(false);
  let redirects = $state<string[]>([""]);
  let implied = $state<string[]>([]);
  let passwordSignUp = $state(false);
  let providerSignUp = $state(false);
  let catalog = $state.raw<apis.auth.CapabilitiesListOutput["items"]>([]);
  // What this Trellis allows; unknown when the viewer can't read it.
  let signIn = $state.raw<apis.auth.SignInSettingsGetOutput["settings"] | null>(
    null,
  );
  const web = $derived(kind === "browser");

  async function loadSignIn(): Promise<void> {
    if (!canPerform(authority.authority, "signInSettingsGet")) return;
    signIn = (await trellis.signInSettingsGet({}).orThrow()).settings;
  }

  async function loadCatalog(): Promise<void> {
    const result = await traverseAll(async (page) => {
      const response = await trellis.capabilitiesList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    catalog = [...result.items];
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    try {
      const response = await trellis
        .oAuthClientsList({ page: { cursor, limit: 50 } })
        .orThrow();
      if (!scope.isCurrent(token)) return;
      clients = response.items;
      next = response.page.nextCursor;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  function edit(client: Client | null): void {
    selected = client;
    clientId = client?.clientId ?? "";
    title = client?.title ?? "";
    description = client?.description ?? "";
    kind = client?.kind ?? "browser";
    development = client?.development ?? false;
    redirects = client?.redirectUris.length ? [...client.redirectUris] : [""];
    implied = [...(client?.impliedConsent ?? [])];
    passwordSignUp = client?.signUp.password ?? false;
    providerSignUp = client?.signUp.providers ?? false;
    editing = true;
    error = null;
    notice = null;
  }

  async function mutate(
    operation: "put" | "disable" | "delete",
  ): Promise<void> {
    if (saving) return;
    error = null;
    notice = null;
    saving = true;
    const token = scope.begin();
    try {
      const identity = {
        clientId,
        expectedRevision: selected?.revision ?? 0n,
        idempotencyKey: ulid(),
      };
      if (operation === "put") {
        const input = apis.auth.API.actions[
          "rpc:OAuthClients.Put"
        ].input.decode({
          ...identity,
          expectedRevision: identity.expectedRevision.toString(),
          title: title.trim(),
          description,
          kind,
          // Native apps sign in with a code, and only web apps skip asking.
          redirectUris: web
            ? redirects.map((uri) => uri.trim()).filter(Boolean)
            : [],
          development: web && development,
          impliedConsent: web ? implied : [],
        });
        let { client } = await trellis.oAuthClientsPut(input).orThrow();
        // Sign-up has its own setting, so re-registering never changes it.
        if (
          client.signUp.password !== passwordSignUp ||
          client.signUp.providers !== providerSignUp
        )
          ({ client } = await trellis
            .oAuthClientsSignUpUpdate({
              clientId,
              signUp: { password: passwordSignUp, providers: providerSignUp },
              expectedRevision: client.revision,
              idempotencyKey: ulid(),
            })
            .orThrow());
        if (!scope.isCurrent(token)) return;
        selected = client;
      } else {
        if (
          !selected ||
          !window.confirm(
            `${operation === "disable" ? "Turn off" : "Delete"} ${title || identity.clientId}?`,
          )
        )
          return;
        if (operation === "disable")
          await trellis.oAuthClientsDisable(identity).orThrow();
        else await trellis.oAuthClientsDelete(identity).orThrow();
        if (!scope.isCurrent(token)) return;
        editing = false;
      }
      notice =
        operation === "put"
          ? "App saved."
          : operation === "disable"
            ? "App turned off."
            : "App deleted.";
      await load();
    } catch (cause) {
      if (scope.isCurrent(token))
        error = isUncertainError(cause)
          ? "We couldn't confirm the change. Reload before trying again."
          : projectConsoleError(cause).message;
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    void load();
    void loadCatalog().catch(() => {});
    void loadSignIn().catch(() => {});
    return () => scope.dispose();
  });
</script>

<section class="space-y-5">
  <PageToolbar
    title="Apps"
    description="Apps people can sign in to with Trellis."
  >
    {#snippet actions()}
      {#if canPerform(authority.authority, "oAuthClientsPut")}<button
          class="btn btn-primary"
          disabled={saving}
          onclick={() => edit(null)}
          ><Icon name="plus" size={18} />New app</button
        >{/if}
    {/snippet}
  </PageToolbar>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}

  <div
    class={[
      "grid gap-4",
      editing && "xl:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]",
    ]}
  >
    <div class="trellis-section min-w-0">
      {#if loading}<LoadingState
          label="Loading apps"
        />{:else if !clients.length}
        <EmptyState
          class="m-4"
          title="No apps yet"
          description="Register an app so people can sign in to it."
        />
      {:else}
        <DataTable>
          <thead>
            <tr>
              <th>App</th>
              <th>Type</th>
              <th>Status</th>
              <th><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each clients as client (client.clientId)}
              <tr
                class={selected?.clientId === client.clientId && editing
                  ? "bg-primary/10"
                  : ""}
              >
                <td>
                  <div class="flex items-center gap-3">
                    <span
                      class="grid h-9 w-9 place-items-center rounded-field bg-primary text-sm font-semibold text-primary-content"
                      aria-hidden="true"
                      >{client.title.charAt(0).toUpperCase()}</span
                    >
                    <span>
                      <span class="block font-medium">{client.title}</span>
                      <span class="block text-xs text-base-content/60"
                        >{client.clientId}</span
                      >
                    </span>
                  </div>
                </td>
                <td class="text-base-content/75"
                  >{client.kind === "native"
                    ? "Native app"
                    : "Web app"}{client.development
                    ? " · local development"
                    : ""}</td
                >
                <td>
                  <span
                    class={[
                      "badge badge-sm",
                      client.enabled ? "badge-success" : "badge-neutral",
                    ]}>{client.enabled ? "Active" : "Disabled"}</span
                  >
                </td>
                <td class="text-right">
                  <button
                    class="btn btn-ghost btn-sm"
                    disabled={saving}
                    onclick={() => edit(client)}>Edit</button
                  >
                </td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
        {#if history.length || next}
          <Pager
            summary={`Showing ${clients.length} apps on this page`}
            canPrevious={history.length > 0}
            canNext={!!next}
            busy={loading || saving}
            onPrevious={() => {
              cursor = history.at(-1);
              history = history.slice(0, -1);
              void load();
            }}
            onNext={() => {
              history = [...history, cursor];
              cursor = next;
              void load();
            }}
          />
        {/if}
      {/if}
    </div>

    {#if editing}
      <form
        class="trellis-section space-y-4 p-5"
        onsubmit={(event) => {
          event.preventDefault();
          void mutate("put");
        }}
      >
        <div class="flex items-center justify-between">
          <h2 class="text-lg font-semibold">
            {selected ? selected.title : "New app"}
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
        <fieldset class="space-y-6" disabled={saving}>
          <div class="space-y-4">
            <label class="trellis-field"
              ><span class="trellis-field-label">Name</span><input
                class="input w-full"
                bind:value={title}
                required
              /></label
            >
            <label class="trellis-field"
              ><span class="trellis-field-label">Description</span><textarea
                class="textarea w-full"
                rows="2"
                bind:value={description}
              ></textarea></label
            >
            {#if selected}
              <div class="trellis-field">
                <span class="trellis-field-label">App ID</span>
                <code class="text-sm">{clientId}</code>
              </div>
            {:else}
              <label class="trellis-field"
                ><span class="trellis-field-label">App ID</span><input
                  class="input w-full font-mono"
                  bind:value={clientId}
                  required
                /><span class="trellis-field-help"
                  >The app uses this to identify itself. It can't change later.</span
                ></label
              >
            {/if}
          </div>

          <div class="space-y-3 border-t border-base-300 pt-5">
            <h3 class="font-semibold">How it signs in</h3>
            <div class="grid gap-2 sm:grid-cols-2" role="radiogroup">
              {#each [{ value: "browser", label: "Web app", help: "Runs in a browser and returns to a web address." }, { value: "native", label: "Native app", help: "Runs on a computer or device and signs in with a code." }] as option (option.value)}
                <label
                  class={[
                    "flex cursor-pointer gap-3 rounded-box border p-3",
                    kind === option.value
                      ? "border-primary bg-primary/5"
                      : "border-base-300",
                  ]}
                  ><input
                    type="radio"
                    class="radio radio-sm mt-0.5"
                    name="kind"
                    value={option.value}
                    bind:group={kind}
                    disabled={selected !== null}
                  /><span
                    ><span class="block font-medium">{option.label}</span><span
                      class="block text-sm text-base-content/60"
                      >{option.help}</span
                    ></span
                  ></label
                >
              {/each}
            </div>
            {#if web}
              <div class="trellis-field">
                <span class="trellis-field-label">Return addresses</span>
                <span class="trellis-field-help"
                  >Where people come back to after signing in.</span
                >
                {#each redirects as _, index (index)}
                  <div class="flex gap-2">
                    <input
                      class="input w-full font-mono text-sm"
                      type="url"
                      aria-label={`Return address ${index + 1}`}
                      placeholder="https://app.example.com/callback"
                      bind:value={redirects[index]}
                    />
                    {#if redirects.length > 1}<button
                        type="button"
                        class="btn btn-ghost btn-square"
                        aria-label={`Remove return address ${index + 1}`}
                        onclick={() =>
                          (redirects = redirects.filter((_, i) => i !== index))}
                        ><Icon name="close" size={16} /></button
                      >{/if}
                  </div>
                {/each}
                <button
                  type="button"
                  class="btn btn-ghost btn-sm self-start"
                  onclick={() => (redirects = [...redirects, ""])}
                  ><Icon name="plus" size={16} />Add address</button
                >
              </div>
              <label class="flex items-start gap-3"
                ><input
                  class="toggle toggle-sm mt-0.5"
                  type="checkbox"
                  bind:checked={development}
                /><span
                  ><span class="block">Local development</span><span
                    class="block text-sm text-base-content/60"
                    >Allows http://localhost return addresses.</span
                  ></span
                ></label
              >
            {/if}
          </div>

          {#if web}
            <div class="space-y-3 border-t border-base-300 pt-5">
              <div>
                <h3 class="font-semibold">Allowed without asking</h3>
                <p class="text-sm text-base-content/60">
                  People who hold these aren't asked to approve them. When this
                  covers everything the app asks for, signing in asks nothing.
                </p>
              </div>
              <PermissionGroups
                capabilities={catalog}
                bind:selected={implied}
              />
            </div>
          {/if}

          {#if canPerform(authority.authority, "oAuthClientsSignUpUpdate")}
            <div class="space-y-3 border-t border-base-300 pt-5">
              <div>
                <h3 class="font-semibold">Creating accounts</h3>
                <p class="text-sm text-base-content/60">
                  New accounts get the roles marked for new users.
                </p>
              </div>
              {#each [{ label: "With a username and password", allowed: signIn?.localSignUp, off: "Account creation is turned off for this Trellis." }, { label: "With a company sign-in", allowed: signIn?.providers, off: "Company sign-in is turned off for this Trellis." }] as option, index (option.label)}
                <label class="flex items-start gap-3"
                  >{#if index === 0}<input
                      class="toggle toggle-sm mt-0.5"
                      type="checkbox"
                      disabled={option.allowed === false}
                      bind:checked={passwordSignUp}
                    />{:else}<input
                      class="toggle toggle-sm mt-0.5"
                      type="checkbox"
                      disabled={option.allowed === false}
                      bind:checked={providerSignUp}
                    />{/if}<span
                    ><span class="block">{option.label}</span
                    >{#if option.allowed === false}<span
                        class="block text-sm text-base-content/60"
                        >{option.off}
                        <a class="link" href={resolve("/admin/sign-in")}
                          >Sign-in methods</a
                        ></span
                      >{/if}</span
                  ></label
                >
              {/each}
            </div>
          {/if}
        </fieldset>
        <div
          class="flex flex-wrap justify-between gap-2 border-t border-base-300 pt-4"
        >
          <div class="flex gap-2">
            {#if selected && canPerform(authority.authority, "oAuthClientsDisable")}<button
                type="button"
                class="btn btn-outline"
                disabled={saving || !selected.enabled}
                onclick={() => void mutate("disable")}>Turn off</button
              >{/if}
            {#if selected && canPerform(authority.authority, "oAuthClientsDelete")}<button
                type="button"
                class="btn btn-outline btn-error"
                disabled={saving}
                onclick={() => void mutate("delete")}>Delete app</button
              >{/if}
          </div>
          {#if canPerform(authority.authority, "oAuthClientsPut")}<button
              class="btn btn-primary"
              disabled={saving}>Save app</button
            >{/if}
        </div>
      </form>
    {/if}
  </div>
</section>
