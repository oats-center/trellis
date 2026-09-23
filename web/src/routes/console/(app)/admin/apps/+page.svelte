<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import { onMount } from "svelte";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { isErr } from "@oats-center/result";

  type GrantBinding = apis.auth.GrantsListOutput["items"][number];

  const trellis = getTrellis();

  let loading = $state(true);
  let error = $state<string | null>(null);
  let identityGrants = $state.raw<GrantBinding[]>([]);

  async function load() {
    loading = true;
    error = null;
    const result = await traverseAll<GrantBinding>(async (pageRequest) => {
      const response = await trellis.grantsList({
        ownerKind: "user",
        page: catalogPage(pageRequest.cursor),
      }).take();
      if (isErr(response)) throw response;
      return { items: response.items, cursor: response.page.nextCursor };
    });
    loading = false;
    if (!result.complete) {
      error = errorMessage(result.error);
      return;
    }
    identityGrants = [...result.items];
  }

  onMount(load);
</script>

<section class="space-y-4">
  <PageToolbar
    title="User-owned grants"
    description="Inspect and revoke grant bindings owned by a user, such as delegated app and agent grants."
  >
    {#snippet actions()}
      <div class="trellis-filterbar-actions">
        <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading}>
          Refresh
        </button>
        <ActionMenu buttonBaseClass="btn btn-outline btn-sm" widthClass="w-64">
          {#snippet summary()}
            Actions <Icon name="chevronDown" size={14} />
          {/snippet}
          <li>
            <a href={resolve("/admin/apps/revoke")}>Revoke user-owned grant</a>
          </li>
        </ActionMenu>
      </div>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading user-owned grants" /></Panel>
  {:else if identityGrants.length === 0}
    <EmptyState
      title="No user-owned grants"
      description="No user-owned grant bindings are currently visible."
    />
  {:else}
    <Panel title="User-owned grants" eyebrow="Primary table">
      <DataTable>
        <thead>
          <tr>
            <th>Owner</th>
            <th>Participant</th>
            <th>State</th>
            <th>Installed revision</th>
            <th>Grant revision</th>
            <th>Updated</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each identityGrants as entry (`${entry.ownerId}:${entry.participantId}`)}
            <tr>
              <td class="trellis-identifier font-medium">{entry.ownerId}</td>
              <td class="trellis-identifier">{entry.participantId}</td>
              <td>
                <span class="badge badge-sm {entry.state === "active" ? "badge-success" : "badge-neutral"}">
                  {entry.state}
                </span>
              </td>
              <td class="trellis-identifier text-base-content/60">{entry.installedRevision}</td>
              <td class="trellis-identifier text-base-content/60">{entry.revision}</td>
              <td class="text-base-content/60">{formatDate(entry.updatedAt)}</td>
              <td class="text-right">
                {#if entry.state === "active"}
                  <ActionMenu>
                    <li>
                      <a
                        class="text-error"
                        href={consoleUrl("/admin/apps/revoke", {
                          query: {
                            ownerKind: "user",
                            ownerId: entry.ownerId,
                            participantId: entry.participantId,
                          },
                        })}>Revoke</a
                      >
                    </li>
                  </ActionMenu>
                {:else}
                  <span class="text-xs text-base-content/50">retained</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </DataTable>
      <p class="text-xs text-base-content/50">
        {identityGrants.length} user-owned grant{identityGrants.length !== 1 ? "s" : ""} loaded
      </p>
    </Panel>
  {/if}
</section>
