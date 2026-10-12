<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { ulid } from "ulid";
  import type { apis } from "trellis-web-generated";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { tablePage, TABLE_PAGE_LIMIT } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import {
    nextCursorPage,
    previousCursorPage,
    resetCursorHistory,
    type CursorHistory,
  } from "$lib/cursor_history.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";

  type Resource = apis.auth.ResourcesListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("resources:detached");
  let filter = $state<"detached" | "all">("detached");
  let resources = $state.raw<Resource[]>([]);
  let history = $state(resetCursorHistory());
  let nextCursor = $state<string>();
  let loading = $state(true);
  let readError = $state<string | null>(null);
  let actionMessage = $state<string | null>(null);
  let busy = $state(false);
  let uncertain = $state(false);
  let confirmation: ConfirmationModal | undefined = $state();
  const authorityState = $derived(authority.state);
  const authorityDigest = $derived(authority.authority?.authorityDigest);
  const canRead = $derived(canPerform(authority.authority, "resourcesList"));
  const canRemove = $derived(
    canPerform(authority.authority, "resourcesRemove"),
  );

  async function load(pageHistory: CursorHistory = history): Promise<void> {
    if (authorityState !== "ready" || !canRead) return;
    const token = scope.begin();
    const state = filter;
    loading = true;
    readError = null;
    try {
      const result = await trellis
        .resourcesList(
          {
            page: tablePage(pageHistory.cursor),
            ...(state === "detached" ? { state: "detached" as const } : {}),
          },
          { timeout: 10_000 },
        )
        .orThrow();
      if (!scope.isCurrent(token)) return;
      resources = result.items;
      nextCursor = result.page.nextCursor;
      history = pageHistory;
    } catch (cause) {
      if (scope.isCurrent(token))
        readError = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  function changeFilter(next: "detached" | "all"): void {
    if (next === filter || busy) return;
    filter = next;
    scope.setKey(`resources:${next}`);
    resources = [];
    nextCursor = undefined;
    history = resetCursorHistory();
    void load();
  }

  async function remove(resource: Resource): Promise<void> {
    if (
      busy ||
      uncertain ||
      loading ||
      readError ||
      resource.state !== "detached" ||
      !canRemove
    )
      return;
    const intent = Object.freeze({
      input: Object.freeze({
        resourceId: resource.resourceId,
        expectedRevision: resource.revision,
        idempotencyKey: ulid(),
      }) satisfies apis.auth.ResourcesRemoveInput,
      name: resource.localName,
      owner: `${resource.ownerKind} ${resource.ownerId}`,
      route: scope.key,
      authority: authority.authority?.authorityDigest,
    });
    busy = true;
    actionMessage = null;
    try {
      const confirmed = await confirmation?.confirm({
        title: "Permanently delete this resource?",
        message:
          "This permanently deletes the retained allocation and its data. This cannot be undone. Keeping it allows a compatible declaration to reattach later.",
        confirmLabel: "Permanently delete",
        targetLabel: "Selected resource and owner",
        targetName: `${intent.name} · ${intent.owner}`,
        expectedValue: intent.name,
        details: `Resource ID: ${intent.input.resourceId} · Revision: ${intent.input.expectedRevision}`,
      });
      if (
        !confirmed ||
        scope.disposed ||
        scope.key !== intent.route ||
        authority.authority?.authorityDigest !== intent.authority ||
        !canPerform(authority.authority, "resourcesRemove")
      )
        return;
      await trellis
        .resourcesRemove(intent.input, { timeout: 10_000 })
        .orThrow();
      if (scope.disposed) return;
      actionMessage = `Deletion requested for ${intent.name}. Refresh to inspect its current state.`;
      await load();
    } catch (cause) {
      if (scope.disposed) return;
      const outcome = classifyMutationError(cause);
      uncertain = outcome.kind === "unknown";
      actionMessage =
        outcome.kind === "conflict"
          ? "The resource changed before deletion. No retry was sent. Refresh and review its current state before making another choice."
          : uncertain
            ? "Deletion outcome unknown. No retry was sent. Refresh to inspect the allocation; deletion controls remain disabled for this page visit."
            : projectConsoleError(cause).message;
      if (outcome.kind === "conflict" || uncertain) await load();
    } finally {
      if (!scope.disposed) busy = false;
    }
  }

  $effect(() => {
    const ready = authorityState === "ready" && authorityDigest !== undefined;
    const permitted = canRead;
    untrack(() => {
      scope.invalidate();
      if (!ready) {
        loading = authorityState === "checking";
        return;
      }
      if (!permitted) {
        resources = [];
        nextCursor = undefined;
        loading = false;
        readError = "You do not have permission to read resources.";
        return;
      }
      void load();
    });
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar
    title="Resources"
    description="Removing a declaration retains its allocation and data. Compatible declarations can reattach; only an explicit administrator deletion permanently removes data."
  >
    {#snippet actions()}
      <button
        class="btn btn-ghost btn-sm"
        disabled={loading || busy || !canRead}
        onclick={() => void load()}>Refresh</button
      >
    {/snippet}
  </PageToolbar>

  {#if actionMessage}<Notice variant={uncertain ? "warning" : "info"}
      >{actionMessage}</Notice
    >{/if}
  {#if readError}<Notice variant="error"
      >{readError} Any displayed rows are the last successful page; deletion is disabled
      until a successful refresh.</Notice
    >{/if}

  <Panel>
    <div class="mb-3 flex flex-wrap items-center justify-between gap-3">
      <div class="join" role="group" aria-label="Resource state filter">
        <button
          class="btn btn-outline btn-sm join-item"
          class:btn-active={filter === "detached"}
          aria-pressed={filter === "detached"}
          disabled={busy}
          onclick={() => changeFilter("detached")}>Detached</button
        >
        <button
          class="btn btn-outline btn-sm join-item"
          class:btn-active={filter === "all"}
          aria-pressed={filter === "all"}
          disabled={busy}
          onclick={() => changeFilter("all")}>All retained states</button
        >
      </div>
      <p class="text-xs text-base-content/60" aria-live="polite">
        {loading
          ? "Loading resources…"
          : `${resources.length} on this page · page size ${TABLE_PAGE_LIMIT}`}
      </p>
    </div>

    {#if loading && resources.length === 0}
      <LoadingState label="Loading resources" />
    {:else if resources.length === 0 && !readError}
      <EmptyState
        title={filter === "detached"
          ? "No detached resources"
          : "No retained resources"}
        description={filter === "detached"
          ? "No detached resources. Removing a declaration retains its allocation here until an administrator explicitly deletes it."
          : "Retained allocations will appear here independently of active deployment declarations."}
      />
    {:else if resources.length > 0}
      {#if loading}<p class="mb-2 text-xs text-base-content/60">
          Refreshing; showing the last successful page. Deletion is disabled.
        </p>{/if}
      <DataTable fixed tableClass="min-w-[72rem]">
        <thead
          ><tr
            ><th class="w-44">Resource</th><th class="w-64">Owner</th><th
              class="w-64">Participant</th
            ><th class="w-96">Resource ID</th><th class="w-32">State</th><th
              class="w-24">Revision</th
            ><th class="w-36">Action</th></tr
          ></thead
        >
        <tbody>
          {#each resources as resource (resource.resourceId)}
            <tr class="hover:bg-base-200/60">
              <td
                ><span class="trellis-identifier block overflow-x-auto"
                  >{resource.localName}</span
                ><span class="text-xs text-base-content/60"
                  >{resource.kind}</span
                ></td
              >
              <td
                ><span class="block text-xs text-base-content/60"
                  >{resource.ownerKind}</span
                ><span class="trellis-identifier block overflow-x-auto text-xs"
                  >{resource.ownerId}</span
                ></td
              >
              <td
                ><span class="trellis-identifier block overflow-x-auto text-xs"
                  >{resource.participantId}</span
                ></td
              >
              <td
                ><span class="trellis-identifier block overflow-x-auto text-xs"
                  >{resource.resourceId}</span
                ></td
              >
              <td
                ><StatusBadge
                  label={resource.state}
                  status={resource.state === "ready"
                    ? "healthy"
                    : resource.state === "failed"
                      ? "unhealthy"
                      : resource.state === "pending" ||
                          resource.state === "destroying"
                        ? "degraded"
                        : "offline"}
                /></td
              >
              <td class="trellis-identifier">{resource.revision}</td>
              <td
                >{#if resource.state === "detached" && canRemove}<button
                    class="btn btn-outline btn-error btn-sm"
                    aria-label={`Delete ${resource.localName} owned by ${resource.ownerKind} ${resource.ownerId}`}
                    disabled={busy ||
                      uncertain ||
                      loading ||
                      readError !== null}
                    onclick={() => void remove(resource)}>Delete</button
                  >{:else}<span class="text-xs text-base-content/50"
                    >{resource.state === "detached"
                      ? "Read only"
                      : "Not detached"}</span
                  >{/if}</td
              >
            </tr>
          {/each}
        </tbody>
      </DataTable>
    {/if}

    {#if resources.length > 0 || history.back.length > 0 || nextCursor !== undefined}
      <div class="mt-3 flex justify-end gap-2">
        <button
          class="btn btn-outline btn-sm"
          disabled={loading || busy || history.back.length === 0}
          onclick={() => void load(previousCursorPage(history))}
          >Previous</button
        >
        <button
          class="btn btn-outline btn-sm"
          disabled={loading || busy || nextCursor === undefined}
          onclick={() => {
            if (nextCursor !== undefined)
              void load(nextCursorPage(history, nextCursor));
          }}>Next</button
        >
      </div>
    {/if}
  </Panel>
</section>

<ConfirmationModal bind:this={confirmation} />
