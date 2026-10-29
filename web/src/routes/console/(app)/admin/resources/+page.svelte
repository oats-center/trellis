<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { ulid } from "ulid";
  import type { apis } from "trellis-web-generated";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { tablePage, traverseAll } from "$lib/console/paging.ts";
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
  import Pager from "$lib/components/Pager.svelte";
  import { humanizeName } from "$lib/format";

  type Resource = apis.auth.ResourcesListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("resources");
  let open = $state<string | null>(null);
  const KINDS: Record<string, string> = {
    kv: "Key-value store",
    store: "File store",
    state: "Saved state",
    job: "Job queue",
    eventConsumer: "Event subscription",
  };
  // Owner names make "used by" readable; IDs are the fallback.
  let ownerNames = $state.raw<Map<string, string>>(new Map());

  async function loadOwners(): Promise<void> {
    const listed = await traverseAll(async (page) => {
      const response = await trellis.deploymentsList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    ownerNames = new Map(
      listed.items.map((item) => [item.deploymentId, humanizeName(item.title)]),
    );
  }
  let resources = $state.raw<Resource[]>([]);
  /** Every unused allocation, so each one can be decided on in one place. */
  let unused = $state.raw<readonly Resource[]>([]);
  let unusedComplete = $state(true);
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
    loading = true;
    readError = null;
    try {
      const [result, detached] = await Promise.all([
        trellis
          .resourcesList(
            { page: tablePage(pageHistory.cursor) },
            { timeout: 10_000 },
          )
          .orThrow(),
        traverseAll(async (page) => {
          const response = await trellis
            .resourcesList(
              { page, state: "detached" as const },
              { timeout: 10_000 },
            )
            .orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
      ]);
      if (!scope.isCurrent(token)) return;
      if (!detached.complete && detached.items.length === 0)
        throw detached.error;
      resources = result.items;
      nextCursor = result.page.nextCursor;
      history = pageHistory;
      unused = detached.items;
      unusedComplete = detached.complete;
    } catch (cause) {
      if (scope.isCurrent(token))
        readError = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
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
        title: "Permanently delete this storage?",
        message:
          "Its data is deleted and can't be recovered. If you keep it, the service can use it again when it comes back.",
        confirmLabel: "Permanently delete",
        targetLabel: "Storage",
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
      void loadOwners().catch(() => {});
    });
  });
  onDestroy(() => scope.dispose());
</script>

{#snippet nameCell(resource: Resource)}
  <button
    type="button"
    class="text-left"
    aria-expanded={open === resource.resourceId}
    onclick={() =>
      (open = open === resource.resourceId ? null : resource.resourceId)}
  >
    <span class="block font-medium hover:underline"
      >{humanizeName(resource.localName)}</span
    >
  </button>
  <span class="block text-xs text-base-content/60"
    >{KINDS[resource.kind] ?? humanizeName(resource.kind)} ·
    <span class="trellis-identifier text-[0.7rem]">{resource.resourceId}</span
    ></span
  >
{/snippet}

{#snippet ownerCell(resource: Resource)}
  <span class="block"
    >{ownerNames.get(resource.ownerId) ??
      humanizeName(resource.participantId ?? resource.clientId ?? "—")}</span
  >
  <span class="block text-xs text-base-content/60"
    >{resource.ownerKind === "user" ? "Person" : "Service"}</span
  >
{/snippet}

{#snippet details(resource: Resource, columns: number)}
  <tr class="bg-base-200/40">
    <td colspan={columns}>
      <dl class="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-[max-content_1fr]">
        <dt class="text-base-content/60">Type</dt>
        <dd>{KINDS[resource.kind] ?? humanizeName(resource.kind)}</dd>
        <dt class="text-base-content/60">Name in its program</dt>
        <dd><code>{resource.localName}</code></dd>
        <dt class="text-base-content/60">Owner</dt>
        <dd>
          {ownerNames.get(resource.ownerId) ?? resource.ownerId}
          <span class="text-base-content/60"
            >· {resource.ownerKind === "user" ? "Person" : "Service"}</span
          >
        </dd>
        {#if resource.participantId}
          <dt class="text-base-content/60">Program</dt>
          <dd><code>{resource.participantId}</code></dd>
        {/if}
        {#if resource.clientId}
          <dt class="text-base-content/60">App</dt>
          <dd><code>{resource.clientId}</code></dd>
        {/if}
        {#if resource.browserOrigin}
          <dt class="text-base-content/60">Web address</dt>
          <dd>{resource.browserOrigin}</dd>
        {/if}
        <dt class="text-base-content/60">Changes</dt>
        <dd>{resource.revision}</dd>
      </dl>
    </td>
  </tr>
{/snippet}

<section class="space-y-5">
  <PageToolbar
    title="Resources"
    description="Storage that services and devices use. Unused storage stays until you delete it."
  >
    {#snippet actions()}
      <button
        class="btn btn-ghost"
        disabled={loading || busy || !canRead}
        onclick={() => void load()}>Refresh</button
      >
    {/snippet}
  </PageToolbar>

  {#if actionMessage}<Notice variant={uncertain ? "warning" : "info"}
      >{actionMessage}</Notice
    >{/if}
  {#if readError}<Notice variant="error"
      >{readError} Deleting is off until the list loads again.</Notice
    >{/if}

  {#if unused.length > 0}
    <section
      class="trellis-section unused-storage"
      aria-labelledby="unused-storage-title"
    >
      <div class="p-4 pb-2">
        <h2 id="unused-storage-title" class="font-semibold">
          Unused storage <span class="badge badge-warning badge-sm"
            >{unused.length}{unusedComplete ? "" : "+"}</span
          >
        </h2>
        <p class="mt-1 text-sm text-base-content/70">
          Nothing uses these any more, usually because the service or device
          that had them was removed or changed. Their data is kept until you
          delete them; a service that comes back can use its storage again.
        </p>
      </div>
      <DataTable>
        <thead>
          <tr>
            <th>Name</th>
            <th>Was used by</th>
            <th><span class="sr-only">Actions</span></th>
          </tr>
        </thead>
        <tbody>
          {#each unused as resource (resource.resourceId)}
            <tr>
              <td>{@render nameCell(resource)}</td>
              <td>{@render ownerCell(resource)}</td>
              <td class="text-right">
                {#if canRemove}<button
                    class="btn btn-outline btn-error btn-sm"
                    aria-label={`Delete ${resource.localName} used by ${resource.ownerKind} ${resource.ownerId}`}
                    disabled={busy ||
                      uncertain ||
                      loading ||
                      readError !== null}
                    onclick={() => void remove(resource)}>Delete</button
                  >{/if}
              </td>
            </tr>
            {#if open === resource.resourceId}
              {@render details(resource, 3)}
            {/if}
          {/each}
        </tbody>
      </DataTable>
    </section>
  {/if}

  <section class="trellis-section" aria-labelledby="all-storage-title">
    <div class="flex flex-wrap items-center justify-between gap-3 p-4">
      <h2 id="all-storage-title" class="font-semibold">All storage</h2>
      <p class="text-sm text-base-content/60" aria-live="polite">
        {loading ? "Loading…" : ""}
      </p>
    </div>

    {#if loading && resources.length === 0}
      <LoadingState label="Loading resources" />
    {:else if resources.length === 0 && !readError}
      <EmptyState
        class="m-4"
        title="No storage yet"
        description="Storage appears when services and devices are set up."
      />
    {:else if resources.length > 0}
      <DataTable>
        <thead>
          <tr>
            <th>Name</th>
            <th>Used by</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {#each resources as resource (resource.resourceId)}
            <tr>
              <td>{@render nameCell(resource)}</td>
              <td>{@render ownerCell(resource)}</td>
              <td>
                <span
                  class={[
                    "badge badge-sm",
                    resource.state === "ready"
                      ? "badge-success"
                      : resource.state === "failed"
                        ? "badge-error"
                        : resource.state === "pending" ||
                            resource.state === "destroying" ||
                            resource.state === "detached"
                          ? "badge-warning"
                          : "badge-neutral",
                  ]}
                  >{resource.state === "detached"
                    ? "Unused"
                    : resource.state === "ready"
                      ? "In use"
                      : resource.state === "destroying"
                        ? "Deleting"
                        : humanizeName(resource.state)}</span
                >
              </td>
            </tr>
            {#if open === resource.resourceId}
              {@render details(resource, 3)}
            {/if}
          {/each}
        </tbody>
      </DataTable>
    {/if}

    {#if resources.length > 0 || history.back.length > 0 || nextCursor !== undefined}
      <Pager
        summary={`Showing ${resources.length} on this page`}
        canPrevious={history.back.length > 0}
        canNext={nextCursor !== undefined}
        busy={loading || busy}
        onPrevious={() => void load(previousCursorPage(history))}
        onNext={() => {
          if (nextCursor !== undefined)
            void load(nextCursorPage(history, nextCursor));
        }}
      />
    {/if}
  </section>
</section>

<ConfirmationModal bind:this={confirmation} />

<style>
  .unused-storage {
    border-color: color-mix(
      in oklab,
      var(--color-warning) 45%,
      var(--color-base-300)
    );
  }
</style>
