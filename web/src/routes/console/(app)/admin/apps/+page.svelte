<script lang="ts">
  import { groupByArea } from "$lib/console/capability_areas.ts";
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import { humanizeName } from "$lib/format";

  type Delegation = apis.auth.DelegationsListOutput["items"][number];
  type Capability = apis.auth.CapabilitiesListOutput["items"][number];

  const trellis = getTrellis();
  const scope = new RequestScope("app-delegations");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let search = $state("");
  let open = $state<string | null>(null);
  let delegations = $state.raw<Delegation[]>([]);
  // Titles and groups make the allowed access readable; IDs are the fallback.
  let capabilities = $state.raw<Map<string, Capability>>(new Map());
  let userNames = $state.raw<Map<string, string>>(new Map());

  const visible = $derived(
    delegations.filter((entry) =>
      `${appName(entry)} ${userNames.get(entry.principalId) ?? ""}`
        .toLowerCase()
        .includes(search.trim().toLowerCase()),
    ),
  );

  function appName(entry: Delegation): string {
    return humanizeName(entry.binding.clientId);
  }

  function host(origin: string | undefined): string {
    if (!origin) return "Native app";
    try {
      return new URL(origin).host;
    } catch {
      return origin;
    }
  }

  function allowed(entry: Delegation) {
    return entry.choices.filter((choice) => choice.decision === "approved");
  }

  // Group the allowed permissions by API area, as people chose them.
  function groupedAccess(entry: Delegation) {
    return groupByArea(
      allowed(entry).map((choice) => choice.capability.capabilityId),
      (id) => id,
    ).map((group) => ({
      id: group.id,
      title: group.title,
      items: group.items.map((id) => capabilities.get(id)?.title ?? id),
    }));
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    const result = await traverseAll(async (page) => {
      const response = await trellis.delegationsList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    if (!scope.settle(token)) return;
    delegations = [...result.items];
    error = result.complete ? null : projectConsoleError(result.error).message;
    loading = false;
  }

  async function loadNames(): Promise<void> {
    const [catalog, users] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis.capabilitiesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.usersList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    capabilities = new Map(
      catalog.items.map((item) => [item.identity.capabilityId, item]),
    );
    userNames = new Map(
      users.items.map((user) => [user.principalId, user.displayName]),
    );
  }

  onMount(() => {
    void load();
    void loadNames().catch(() => {});
    return () => scope.dispose();
  });
</script>

<section class="space-y-5">
  <PageToolbar
    title="Connected apps"
    description="Apps people have allowed to use their account."
  />
  {#if error}<Notice variant="warning">{error}</Notice>{/if}

  <div class="trellis-section">
    <div class="p-4">
      <label class="input flex w-full max-w-md items-center gap-2 bg-base-100">
        <Icon name="search" size={16} class="text-base-content/50" />
        <input
          class="grow"
          bind:value={search}
          placeholder="Search by app or person"
          aria-label="Search connected apps"
        />
      </label>
    </div>
    {#if loading}
      <LoadingState label="Loading connected apps" />
    {:else if !visible.length}
      <EmptyState
        class="m-4"
        title={delegations.length ? "No matches" : "No connected apps"}
        description={delegations.length
          ? "Try a different search."
          : "Apps show up here after someone allows them."}
      />
    {:else}
      <DataTable>
        <thead>
          <tr>
            <th>App</th>
            <th>Person</th>
            <th>Allowed</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each visible as entry (entry.delegationId)}
            {@const groups = groupedAccess(entry)}
            <tr>
              <td>
                <div class="flex items-center gap-3">
                  <span
                    class="grid h-9 w-9 place-items-center rounded-field bg-primary text-sm font-semibold text-primary-content"
                    aria-hidden="true">{appName(entry).charAt(0)}</span
                  >
                  <span>
                    <span class="block font-medium">{appName(entry)}</span>
                    <span class="block text-xs text-base-content/60"
                      >{host(entry.binding.origin)}</span
                    >
                  </span>
                </div>
              </td>
              <td>{userNames.get(entry.principalId) ?? "Unknown user"}</td>
              <td class="max-w-md whitespace-normal">
                {#if groups.length}
                  <button
                    type="button"
                    class="text-left hover:underline"
                    aria-expanded={open === entry.delegationId}
                    onclick={() =>
                      (open =
                        open === entry.delegationId
                          ? null
                          : entry.delegationId)}
                  >
                    {groups
                      .slice(0, 3)
                      .map((group) => group.title)
                      .join(", ")}{groups.length > 3
                      ? ` and ${groups.length - 3} more`
                      : ""}
                    <span class="text-base-content/55"
                      >· {allowed(entry).length} permissions</span
                    >
                  </button>
                  {#if open === entry.delegationId}
                    <dl class="mt-3 space-y-2 text-sm">
                      {#each groups as group (group.id)}
                        <div>
                          <dt class="font-medium">{group.title}</dt>
                          <dd class="text-base-content/70">
                            {group.items.join(", ")}
                          </dd>
                        </div>
                      {/each}
                    </dl>
                  {/if}
                {:else}
                  <span class="text-base-content/55"
                    >Only what it always needs</span
                  >
                {/if}
              </td>
              <td class="text-right">
                <ActionMenu
                  widthClass="w-44"
                  ariaLabel={`Actions for ${appName(entry)}`}
                >
                  <li>
                    <a
                      class="text-error"
                      href={resolve(
                        `/admin/apps/revoke?delegationId=${entry.delegationId}`,
                      )}>Disconnect</a
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
