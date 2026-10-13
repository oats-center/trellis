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
  import Panel from "$lib/components/Panel.svelte";
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
  let implied = $state("");

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
    implied = client?.impliedConsent.join("\n") ?? "";
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
          impliedConsent: implied.split(/\s+/).filter(Boolean),
        });
        const response = await trellis.oAuthClientsPut(input).orThrow();
        if (!scope.isCurrent(token)) return;
        selected = response.client;
      } else {
        if (
          !selected ||
          !window.confirm(
            `${operation === "disable" ? "Disable" : "Delete"} OAuth client ${identity.clientId}?`,
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
          ? "Client saved."
          : operation === "disable"
            ? "Client disabled."
            : "Client deleted.";
      await load();
    } catch (cause) {
      if (scope.isCurrent(token))
        error = isUncertainError(cause)
          ? "Outcome unknown. Reload the client before another change."
          : projectConsoleError(cause).message;
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="OAuth clients"
    description="Register public browser and native clients with exact redirect URIs."
  >
    {#snippet actions()}
      <button
        class="btn btn-ghost btn-sm"
        disabled={loading || saving}
        onclick={() => void load()}>Refresh</button
      >
      {#if canPerform(authority.authority, "oAuthClientsPut")}<button
          class="btn btn-outline btn-sm"
          disabled={saving}
          onclick={() => edit(null)}>New client</button
        >{/if}
    {/snippet}
  </PageToolbar>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}
  <Panel title="Registered clients" eyebrow={`${clients.length} on this page`}>
    {#if loading}<LoadingState label="Loading clients" />{:else}
      <DataTable
        ><thead
          ><tr
            ><th>Client</th><th>Kind</th><th>State</th><th>Revision</th><th
              >Actions</th
            ></tr
          ></thead
        ><tbody>
          {#each clients as client (client.clientId)}<tr
              ><td
                ><div>{client.title}</div>
                <div class="trellis-identifier">{client.clientId}</div></td
              ><td>{client.kind}{client.development ? " · development" : ""}</td
              ><td
                ><span
                  class="badge badge-sm {client.enabled
                    ? 'badge-success'
                    : 'badge-neutral'}"
                  >{client.enabled ? "Enabled" : "Disabled"}</span
                ></td
              ><td>{client.revision}</td><td
                ><button
                  class="btn btn-ghost btn-xs"
                  disabled={saving}
                  onclick={() => edit(client)}>Edit</button
                ></td
              ></tr
            >{:else}<tr
              ><td colspan="5">No registered clients on this page.</td></tr
            >{/each}
        </tbody></DataTable
      >
    {/if}
    <div class="flex gap-2 p-3">
      <button
        class="btn btn-ghost btn-xs"
        disabled={loading || saving || !history.length}
        onclick={() => {
          cursor = history.at(-1);
          history = history.slice(0, -1);
          void load();
        }}>Previous</button
      ><button
        class="btn btn-ghost btn-xs"
        disabled={loading || saving || !next}
        onclick={() => {
          history = [...history, cursor];
          cursor = next;
          void load();
        }}>Next</button
      >
    </div>
  </Panel>
  {#if editing}<Panel title={selected ? "Edit client" : "New client"}>
      <form
        class="space-y-3"
        onsubmit={(event) => {
          event.preventDefault();
          void mutate("put");
        }}
      >
        <fieldset class="grid gap-3 md:grid-cols-2" disabled={saving}>
          <label class="form-control"
            ><span class="trellis-field-label">Client ID</span><input
              class="input input-bordered input-sm"
              bind:value={clientId}
              required
              readonly={selected !== null}
            /></label
          >
          <label class="form-control"
            ><span class="trellis-field-label">Title</span><input
              class="input input-bordered input-sm"
              bind:value={title}
              required
            /></label
          >
          <label class="form-control"
            ><span class="trellis-field-label">Description</span><input
              class="input input-bordered input-sm"
              bind:value={description}
            /></label
          >
          <label class="form-control"
            ><span class="trellis-field-label">Kind</span><select
              class="select select-bordered select-sm"
              bind:value={kind}
              ><option value="browser">Browser</option><option value="native"
                >Native</option
              ></select
            ></label
          >
          <label class="form-control"
            ><span class="trellis-field-label"
              >Redirect URIs (one per line)</span
            ><textarea class="textarea textarea-bordered" bind:value={redirects}
            ></textarea></label
          >
          <label class="form-control"
            ><span class="trellis-field-label"
              >Preapproved capability IDs (one per line)</span
            ><textarea class="textarea textarea-bordered" bind:value={implied}
            ></textarea><span class="trellis-field-help"
              >Only this explicit subset bypasses consent for eligible flows.</span
            ></label
          >
          <label class="flex items-center gap-2"
            ><input
              class="checkbox checkbox-sm"
              type="checkbox"
              bind:checked={development}
            />Loopback development client</label
          >
        </fieldset>
        <div class="flex flex-wrap gap-2">
          {#if canPerform(authority.authority, "oAuthClientsPut")}<button
              class="btn btn-outline btn-sm"
              disabled={saving}>Save client</button
            >{/if}
          {#if selected && canPerform(authority.authority, "oAuthClientsDisable")}<button
              type="button"
              class="btn btn-outline btn-sm"
              disabled={saving || !selected.enabled}
              onclick={() => void mutate("disable")}>Disable client</button
            >{/if}
          {#if selected && canPerform(authority.authority, "oAuthClientsDelete")}<button
              type="button"
              class="btn btn-outline btn-error btn-sm"
              disabled={saving}
              onclick={() => void mutate("delete")}>Delete client</button
            >{/if}
          <button
            type="button"
            class="btn btn-ghost btn-sm"
            disabled={saving}
            onclick={() => (editing = false)}>Close</button
          >
        </div>
      </form>
    </Panel>{/if}
</section>
