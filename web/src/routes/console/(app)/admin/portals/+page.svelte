<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve, consoleUrl } from "$lib/console_paths";
  import { onMount } from "svelte";
  import { tablePage } from "$lib/console/paging.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController, type MutationIntent } from "$lib/console/mutation.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Portal = {
    portalId: string;
    displayName: string;
    entryUrl: string | null;
    builtIn: boolean;
    disabled: boolean;
    version: bigint;
    routeCount: number;
    activeRouteCount: number;
    createdAt: bigint;
    updatedAt: bigint;
  };

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const removal = new MutationController<apis.auth.PortalsRemoveInput, apis.auth.PortalsRemoveOutput>();
  let disposed = false;
  let loading = $state(true);
  let removingPortalId = $state<string | null>(null);
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);
  let uncertain = $state(false);
  let portals = $state.raw<Portal[]>([]);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const activePortalCount = $derived(portals.filter((portal) => !portal.disabled).length);
  const busy = $derived(loading || removingPortalId !== null || uncertain);

  function closeActionMenus(event: MouseEvent): void {
    if (event.target instanceof Element && event.target.closest("[data-action-menu]")) return;

    for (const menu of document.querySelectorAll<HTMLDetailsElement>("[data-action-menu]")) {
      menu.open = false;
    }
  }

  async function load() {
    loading = true;
    error = null;
    try {
      const portalsResponse = await trellis.portalsList({
        page: tablePage(),
      }).take();
      if (isErr(portalsResponse)) {
        error = errorMessage(portalsResponse);
        return;
      }
      // Built-in state comes from the record, never from display text.
      portals = portalsResponse.items.map((portal) => ({
        ...portal,
        routeCount: 0,
        activeRouteCount: 0,
      }));
    } catch (e) {
      error = errorMessage(e);
    } finally {
      loading = false;
    }
  }

  async function removePortal(intent: MutationIntent<apis.auth.PortalsRemoveInput>) {
    error = null;
    saved = null;
    try {
      const outcome = await removal.send({
        isStillValid: () => !disposed && portals.some((portal) => portal.portalId === intent.targetId && !portal.builtIn &&

            portal.routeCount === 0 && portal.version === intent.input.expectedVersion),
        dispatch: async ({ input }) => await trellis.portalsRemove(input).orThrow(),
      });
      if (!outcome) return;
      if (disposed) {
        uncertain = true;
        error = "Authorization changed during removal. Inspect the portal list and reload before another change.";
        return;
      }
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = "Portal removal outcome unknown. Inspect the portal list and reload before another change.";
        } else error = errorMessage(outcome.error);
        return;
      }
      const response = outcome.value;
      saved = response.removed ? "Portal removed." : "Portal was already absent.";
      await load();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      removingPortalId = null;
    }
  }

  async function requestRemovePortal(portal: Portal) {
    if (busy || portal.builtIn || portal.routeCount > 0 || false) return;

    const key = ulid();
    const intent = captureIntent<apis.auth.PortalsRemoveInput>({
      operation: "portalsRemove", targetId: portal.portalId, label: portal.displayName,
      expectedValue: portal.portalId, idempotencyKey: key,
      input: { portalId: portal.portalId, expectedVersion: portal.version, idempotencyKey: key },
      scope: { routeKey: "portal-list" },
    });
    if (!removal.begin(intent)) return;
    removingPortalId = portal.portalId;
    const confirmed = await confirmationModal?.confirm({
      title: "Delete portal?",
      message: "This removes the portal record. Portal routes must be removed first.",
      confirmLabel: "Delete portal",
      targetLabel: "Portal",
      targetName: portal.portalId,
      expectedValue: portal.portalId,
    });
    if (confirmed) await removePortal(intent);
    else {
      removal.cancel();
      removingPortalId = null;
    }
  }

  onMount(() => {
    void load();
    return () => { disposed = true; };
  });
</script>

<svelte:document onclick={closeActionMenus} />

<section class="space-y-4">
  <PageToolbar title="Portals" description="Manage login portal records and their portal-scoped route rules.">
    {#snippet actions()}
      <button class="btn btn-ghost btn-sm" onclick={load} disabled={busy}>Refresh</button>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}
  {#if saved}
    <Notice variant="success">{saved}</Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading portals" /></Panel>
  {:else}
    <Panel title="Portals" eyebrow={`${activePortalCount} active / ${portals.length} on this page`}>
      {#snippet actions()}
        <a class="btn btn-outline btn-xs" href={resolve("/admin/portals/new")}>New portal</a>
      {/snippet}

      {#if portals.length === 0}
        <EmptyState title="No portals" description="Create a portal, then add route rules from its portal record." />
      {:else}
        <DataTable class="border-b border-base-300 bg-base-100/30" overflow="visible">
            <thead>
              <tr>
                <th>Portal</th>
                <th class="hidden md:table-cell">Entry URL</th>
                <th>Mode</th>
                <th>Status</th>
                <th>Routes</th>
                <th class="hidden lg:table-cell">Updated</th>
                <th class="text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              {#each portals as portal (portal.portalId)}
                <tr>
                  <td>
                    <div class="font-medium">{portal.displayName}</div>
                    <div class="font-mono text-xs text-base-content/55">{portal.portalId}</div>
                  </td>
                  <td class="hidden max-w-[22rem] truncate font-mono text-xs md:table-cell">{portal.entryUrl ?? "built-in"}</td>
                  <td><span class="badge badge-sm {portal.builtIn ? 'badge-info' : 'badge-neutral'}">{portal.builtIn ? "built-in" : "external"}</span></td>
                  <td><span class="badge badge-sm {portal.disabled ? 'badge-neutral' : 'badge-success'}">{portal.disabled ? "disabled" : "active"}</span></td>
                  <td class="font-mono text-xs">{portal.activeRouteCount} / {portal.routeCount}</td>
                  <td class="hidden text-xs text-base-content/60 lg:table-cell">{formatDate(portal.updatedAt)}</td>
                  <td class="text-right">
                    <ActionMenu widthClass="w-44" dataActionMenu>
                        <li><a href={consoleUrl("/admin/portals/edit", { query: { portalId: portal.portalId } })}>Edit</a></li>
                        {#if !portal.builtIn}
                          <li>
                            <button class="text-error" onclick={() => requestRemovePortal(portal)} disabled={busy || portal.routeCount > 0} title={portal.routeCount > 0 ? "Remove routes first" : "Delete portal"}>{removingPortalId === portal.portalId ? "Deleting" : "Delete"}</button>
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
