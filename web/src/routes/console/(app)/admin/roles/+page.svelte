<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("roles");
  let roles = $state.raw<apis.auth.RolesListOutput["items"]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    const result = await traverseAll(async (page) => {
      const response = await trellis.rolesList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    if (!scope.settle(token)) return;
    roles = [...result.items];
    loading = false;
    if (!result.complete) error = projectConsoleError(result.error).message;
  }
  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="Roles"
    description="Manage reusable sets of whole, accepted capabilities."
  >
    {#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load()}>Refresh</button
      >{/snippet}
  </PageToolbar>
  {#if error}<Notice variant="error">Catalog incomplete: {error}</Notice>{/if}
  <Panel title="Roles" eyebrow={`${roles.length} loaded roles`}>
    {#snippet actions()}{#if canPerform(authority.authority, "rolesPut")}<a
          class="btn btn-outline btn-xs"
          href={resolve("/admin/roles/new")}>New role</a
        >{/if}{/snippet}
    {#if loading}<LoadingState
        label="Loading roles"
      />{:else if !roles.length}<EmptyState
        title="No roles returned"
        description="Create a role to bundle whole capabilities."
      />{:else}
      <DataTable
        ><thead
          ><tr
            ><th>Role</th><th>Description</th><th>Capabilities</th><th
              >Generation</th
            ><th>Revision</th><th>Actions</th></tr
          ></thead
        ><tbody>
          {#each roles as role (role.roleId)}<tr
              ><td
                ><div class="font-semibold">{role.title}</div>
                <div class="trellis-identifier">{role.roleId}</div></td
              ><td>{role.description}</td><td>{role.capabilities.length}</td><td
                class="trellis-identifier">{role.identityGeneration}</td
              ><td class="trellis-identifier">{role.revision}</td><td
                ><a
                  class="btn btn-ghost btn-xs"
                  href={resolve(
                    `/admin/roles/edit?roleId=${encodeURIComponent(role.roleId)}`,
                  )}>Edit</a
                ></td
              ></tr
            >{/each}
        </tbody></DataTable
      >
    {/if}
  </Panel>
</section>
