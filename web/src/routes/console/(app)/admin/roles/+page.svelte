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
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import Icon from "$lib/components/Icon.svelte";

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

<section class="space-y-5">
  <PageToolbar
    title="Roles"
    description="Sets of permissions you give to people."
  >
    {#snippet actions()}{#if canPerform(authority.authority, "rolesPut")}<a
          class="btn btn-primary"
          href={resolve("/admin/roles/new")}
          ><Icon name="plus" size={18} />New role</a
        >{/if}{/snippet}
  </PageToolbar>
  {#if error}<Notice variant="warning">{error}</Notice>{/if}
  <div class="trellis-section">
    {#if loading}<LoadingState label="Loading roles" />{:else if !roles.length}
      <EmptyState
        class="m-4"
        title="No roles yet"
        description="Create a role to give people a set of permissions at once."
      />
    {:else}
      <DataTable>
        <thead>
          <tr>
            <th>Role</th>
            <th>Permissions</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each roles as role (role.roleId)}
            <tr>
              <td class="whitespace-normal">
                <a
                  class="font-medium hover:underline"
                  href={resolve(
                    `/admin/roles/edit?roleId=${encodeURIComponent(role.roleId)}`,
                  )}>{role.title}</a
                >{#if role.assignToNewUsers}<span
                    class="badge badge-soft badge-sm ml-2">New users</span
                  >{/if}
                {#if role.description}<div class="text-sm text-base-content/65">
                    {role.description}
                  </div>{/if}
              </td>
              <td class="text-base-content/75">{role.capabilities.length}</td>
              <td class="text-right">
                <ActionMenu
                  widthClass="w-36"
                  ariaLabel={`Actions for ${role.title}`}
                >
                  <li>
                    <a
                      href={resolve(
                        `/admin/roles/edit?roleId=${encodeURIComponent(role.roleId)}`,
                      )}>Edit</a
                    >
                  </li>
                </ActionMenu>
              </td>
            </tr>
          {/each}
        </tbody>
      </DataTable>
    {/if}
  </div>
</section>
