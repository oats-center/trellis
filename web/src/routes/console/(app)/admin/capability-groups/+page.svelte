<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { onMount } from "svelte";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { getTrellis } from "$lib/trellis";

  type CapabilityGroupView = apis.auth.CapabilityGroupsListOutput["items"][number];

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const removal = new MutationController<apis.auth.CapabilityGroupsDeleteInput, apis.auth.CapabilityGroupsDeleteOutput>();
  let disposed = false;

  let loading = $state(true);
  let incomplete = $state(true);
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);
  let deletingGroupKey = $state<string | null>(null);
  let uncertain = $state(false);
  let groups = $state.raw<CapabilityGroupView[]>([]);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const sortedGroups = $derived(groups.slice().sort(compareGroups));
  const busy = $derived(loading || deletingGroupKey !== null || uncertain || incomplete);

  function compareGroups(left: CapabilityGroupView, right: CapabilityGroupView): number {
    if ((left.groupKey === "admin") !== (right.groupKey === "admin")) return left.groupKey === "admin" ? -1 : 1;
    return left.groupKey.localeCompare(right.groupKey);
  }

  function closeActionMenus(event: MouseEvent): void {
    if (event.target instanceof Element && event.target.closest("[data-action-menu]")) return;

    for (const menu of document.querySelectorAll<HTMLDetailsElement>("[data-action-menu]")) {
      menu.open = false;
    }
  }

  async function load() {
    loading = true;
    incomplete = true;
    if (!uncertain) error = null;
    try {
      const result = await traverseAll<CapabilityGroupView>(async (page) => {
        const response = await trellis.capabilityGroupsList({ page: catalogPage(page.cursor) }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!result.complete) throw result.error;
      groups = [...result.items];
      incomplete = false;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  async function requestDeleteGroup(group: CapabilityGroupView) {
    if (busy || group.groupKey === "admin") return;
    const idempotencyKey = ulid();
    const intent = captureIntent<apis.auth.CapabilityGroupsDeleteInput>({
      operation: "capabilityGroupsDelete",
      targetId: group.groupKey,
      label: group.displayName,
      idempotencyKey,
      input: { groupKey: group.groupKey, expectedVersion: group.version, idempotencyKey },
      scope: { routeKey: "capability-groups" },
    });
    if (!removal.begin(intent)) return;
    deletingGroupKey = intent.targetId;
    const confirmed = await confirmationModal?.confirm({
      title: "Delete capability group?",
      message: "This removes the custom capability group from the authority catalog.",
      confirmLabel: "Delete group",
      targetLabel: "Capability group",
      targetName: intent.targetId,
      expectedValue: intent.targetId,
    });
    if (!confirmed) {
      removal.cancel();
      deletingGroupKey = null;
      return;
    }
    error = null;
    saved = null;
    try {
      const outcome = await removal.send({
        isStillValid: () => !disposed && groups.some((candidate) => candidate.groupKey === intent.targetId && candidate.version === intent.input.expectedVersion),

        dispatch: async ({ input }) => await trellis.capabilityGroupsDelete(input).orThrow(),
      });
      if (!outcome) return;
      if (disposed) return;
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = "Group deletion outcome unknown. Inspect the group and reload before another change.";
        } else error = errorMessage(outcome.error);
        return;
      }
      saved = `Capability group ${intent.targetId} deleted.`;
      await load();
    } finally {
      deletingGroupKey = null;
    }
  }

  onMount(() => {
    void load();
    return () => { disposed = true; };
  });
</script>

<svelte:document onclick={closeActionMenus} />

<section class="space-y-4">
  <PageToolbar title="Capability groups" description="Manage reusable auth capability sets and nested group inclusion.">
    {#snippet actions()}
      <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading || deletingGroupKey !== null || uncertain}>Refresh</button>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}
  {#if incomplete && !loading}
    <Notice variant="warning">Group catalog incomplete. Refresh before deleting a listed group.</Notice>
  {/if}
  {#if saved}
    <Notice variant="success">{saved}</Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading capability groups" /></Panel>
  {:else}
    <Panel title="Capability groups" eyebrow={incomplete ? "Catalog incomplete" : `${groups.length} groups`}>
      {#snippet actions()}
        <a class="btn btn-outline btn-xs" href={resolve("/admin/capability-groups/new")}>New group</a>
      {/snippet}

      {#if incomplete && groups.length === 0}
        <EmptyState title="Group catalog unavailable" description="Refresh to load the complete catalog before taking actions." />
      {:else if groups.length === 0}
        <EmptyState title="No capability groups" description="Create a group to bundle common capability assignments." />
      {:else}
        <DataTable class="capability-groups-table border-b border-base-300 bg-base-100/30" overflow="visible">
            <thead>
              <tr>
                <th class="w-[30%]">Group</th>
                <th>Description</th>
                <th class="w-24">Caps</th>
                <th class="w-24">Includes</th>
                <th class="hidden w-32 lg:table-cell">Updated</th>
                <th class="w-24 text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              {#each sortedGroups as group (group.groupKey)}
                <tr>
                  <td class="max-w-0 align-top">
                    <div class="flex min-w-0 items-center gap-2">
                      <span class="trellis-identifier truncate font-semibold" title={group.groupKey}>{group.groupKey}</span>
                      {#if group.groupKey === "admin"}
                        <span class="badge badge-neutral badge-xs shrink-0">built-in</span>
                      {:else}
                        <span class="badge badge-ghost badge-xs shrink-0">custom</span>
                      {/if}
                    </div>
                    <div class="truncate text-xs text-base-content/60" title={group.displayName}>{group.displayName}</div>
                  </td>
                  <td class="max-w-0 align-top text-xs text-base-content/60"><div class="truncate" title={group.description}>{group.description}</div></td>
                  <td class="align-top"><span class="badge badge-ghost badge-sm">{group.capabilities.length}</span></td>
                  <td class="align-top"><span class="badge badge-ghost badge-sm">{group.includedGroups.length}</span></td>
                  <td class="hidden align-top text-xs text-base-content/60 lg:table-cell">{formatDate(group.updatedAt)}</td>
                  <td class="align-top text-right">
                    <ActionMenu widthClass="w-44" dataActionMenu>
                        <li><a href={resolve(`/admin/capability-groups/edit?groupKey=${encodeURIComponent(group.groupKey)}`)}>Edit</a></li>
                        {#if group.groupKey !== "admin"}
                          <li>
                            <button class="text-error" onclick={() => requestDeleteGroup(group)} disabled={busy}>{deletingGroupKey === group.groupKey ? "Deleting" : "Delete"}</button>
                          </li>
                        {/if}
                    </ActionMenu>
                  </td>
                </tr>
              {/each}
            </tbody>
        </DataTable>
      {/if}
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />

<style>
  :global(.capability-groups-table) {
    min-width: 0;
    table-layout: fixed;
    width: 100%;
  }

  :global(.capability-groups-table thead) {
    background-color: color-mix(in oklab, var(--color-base-content) 3.5%, transparent);
  }
</style>
