<script lang="ts">
  import { groupByArea } from "$lib/console/capability_areas.ts";
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
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

  // Own: the signed-in person's apps only, as anyone may see them.
  let { own = false }: { own?: boolean } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("app-delegations");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let search = $state("");
  let groupBy = $state<"none" | "person" | "app">("none");
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

  // Rows under a heading per person or app, in name order.
  const sections = $derived.by(() => {
    if (groupBy === "none") return [{ key: "", title: "", entries: visible }];
    const byKey = new Map<
      string,
      { key: string; title: string; entries: Delegation[] }
    >();
    for (const entry of visible) {
      const key =
        groupBy === "person" ? entry.principalId : entry.binding.clientId;
      const title =
        groupBy === "person"
          ? (userNames.get(entry.principalId) ?? "Unknown user")
          : appName(entry);
      const section = byKey.get(key) ?? { key, title, entries: [] };
      section.entries.push(entry);
      byKey.set(key, section);
    }
    return [...byKey.values()].sort((a, b) => a.title.localeCompare(b.title));
  });

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
      const response = await trellis
        .delegationsList(
          own && authority.profile
            ? { page, principalId: authority.profile.principalId }
            : { page },
        )
        .orThrow();
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
      own
        ? Promise.resolve({ items: [] })
        : traverseAll(async (page) => {
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
    title={own ? "Your apps" : "Connected apps"}
    description={own
      ? "Apps you have allowed to use your account."
      : "Apps people have allowed to use their account."}
  />
  {#if error}<Notice variant="warning">{error}</Notice>{/if}

  <div class="trellis-section">
    <div class="flex flex-wrap items-center gap-3 p-4">
      <label class="input flex w-full max-w-md items-center gap-2 bg-base-100">
        <Icon name="search" size={16} class="text-base-content/50" />
        <input
          class="grow"
          bind:value={search}
          placeholder={own ? "Search your apps" : "Search by app or person"}
          aria-label="Search connected apps"
        />
      </label>
      {#if !own}<div class="join" role="group" aria-label="Group by">
          {#each [{ value: "none", label: "All" }, { value: "person", label: "By person" }, { value: "app", label: "By app" }] as const as option (option.value)}
            <button
              type="button"
              class={[
                "btn join-item btn-sm",
                groupBy === option.value && "btn-active",
              ]}
              aria-pressed={groupBy === option.value}
              onclick={() => (groupBy = option.value)}>{option.label}</button
            >
          {/each}
        </div>{/if}
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
            {#if !own}<th>Person</th>{/if}
            <th>Allowed</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each sections as section (section.key)}
            {#if section.title}
              <tr class="bg-base-200/60">
                <th colspan={own ? 3 : 4} class="text-sm font-semibold">
                  {section.title}
                  <span class="font-normal text-base-content/55"
                    >· {section.entries.length}
                    {groupBy === "app"
                      ? section.entries.length === 1
                        ? "person"
                        : "people"
                      : section.entries.length === 1
                        ? "app"
                        : "apps"}</span
                  >
                </th>
              </tr>
            {/if}
            {#each section.entries as entry (entry.delegationId)}
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
                {#if !own}<td
                    >{userNames.get(entry.principalId) ?? "Unknown user"}</td
                  >{/if}
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
          {/each}
        </tbody>
      </DataTable>
    {/if}
  </div>
</section>
