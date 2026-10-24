<script lang="ts">
  import { onMount } from "svelte";
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
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
  let redirects = $state("");
  let implied = $state<string[]>([]);
  let catalog = $state.raw<apis.auth.CapabilitiesListOutput["items"]>([]);

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
    redirects = client?.redirectUris.join("\n") ?? "";
    implied = [...(client?.impliedConsent ?? [])];
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
          development,
          redirectUris: redirects.split(/\s+/).filter(Boolean),
          impliedConsent: implied,
        });
        const response = await trellis.oAuthClientsPut(input).orThrow();
        if (!scope.isCurrent(token)) return;
        selected = response.client;
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
      editing && "xl:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]",
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
        <fieldset class="space-y-4" disabled={saving}>
          <label class="trellis-field"
            ><span class="trellis-field-label">Name</span><input
              class="input w-full"
              bind:value={title}
              required
            /></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">App ID</span><input
              class="input w-full font-mono"
              bind:value={clientId}
              required
              readonly={selected !== null}
            /><span class="trellis-field-help"
              >The app uses this to identify itself. It can't change later.</span
            ></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">Description</span><input
              class="input w-full"
              bind:value={description}
            /></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">Type</span><select
              class="select w-full"
              bind:value={kind}
              ><option value="browser">Web app</option><option value="native"
                >Native app</option
              ></select
            ></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">Redirect URLs</span><textarea
              class="textarea w-full font-mono text-sm"
              bind:value={redirects}
            ></textarea><span class="trellis-field-help"
              >Where people return after signing in. One per line.</span
            ></label
          >
          <label class="flex items-center gap-3"
            ><input
              class="checkbox checkbox-sm"
              type="checkbox"
              bind:checked={development}
            /><span
              >Local development app <span class="text-sm text-base-content/60"
                >· allows localhost redirect URLs</span
              ></span
            ></label
          >
          <div class="space-y-2">
            <div>
              <span class="trellis-field-label">Allowed without asking</span>
              <p class="trellis-field-help">
                People won't be asked to approve these. Leave empty to always
                ask.
              </p>
            </div>
            <PermissionGroups capabilities={catalog} bind:selected={implied} />
          </div>
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
