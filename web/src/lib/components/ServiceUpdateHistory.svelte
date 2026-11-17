<script lang="ts">
  import type { apis } from "$lib/trellis-apis";
  import { formatDate } from "$lib/format";
  import {
    describeUpdateChanges,
    summarizeUpdateChanges,
  } from "$lib/console/service_update_changes.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";

  type Update = apis.auth.ServiceUpdatesGetOutput["update"];

  // A deployment's decided updates, newest first: when each came, what it
  // changed, how it ended, and who decided.
  let {
    updates,
    titles,
  }: { updates: readonly Update[]; titles: ReadonlyMap<string, string> } =
    $props();

  const LATEST = 10;
  let showAll = $state(false);
  let open = $state<string | null>(null);
  const decided = $derived(
    updates.filter((update) => update.state !== "pending"),
  );
  const shown = $derived(showAll ? decided : decided.slice(0, LATEST));

  const STATES: Record<
    string,
    { label: string; status: "healthy" | "offline" | "degraded" }
  > = {
    applied: { label: "Installed", status: "healthy" },
    rejected: { label: "Rejected", status: "degraded" },
    superseded: { label: "Replaced", status: "offline" },
  };

  function details(update: Update): { label: string; items: string[] }[] {
    const described = describeUpdateChanges(update.changes, titles);
    return [
      { label: "Asked for", items: described.asks },
      { label: "Gave up", items: described.givesUp },
      { label: "APIs", items: described.provides },
    ];
  }

  function decider(update: Update): string {
    if (update.state === "superseded") return "Replaced by a newer build";
    if (!update.decidedBy) return "—";
    return update.decidedBy.kind === "automatic"
      ? "Automatic"
      : (update.decidedBy.name ?? "A user");
  }
</script>

<section
  class="trellis-section"
  aria-labelledby="update-history-title"
  id="update-history"
>
  <div class="flex flex-wrap items-baseline justify-between gap-2 px-5 pt-5">
    <h2 id="update-history-title" class="text-base font-semibold">
      Update history
    </h2>
    {#if decided.length > LATEST}<span
        class="text-sm text-base-content/65 tabular-nums"
        >Showing {shown.length} of {decided.length}</span
      >{/if}
  </div>
  {#if decided.length === 0}
    <p class="px-5 pt-1 pb-5 text-sm text-base-content/65">
      No updates yet. Each new build the service connects with is listed here.
    </p>
  {:else}
    <DataTable class="mt-3">
      <thead>
        <tr>
          <th class="hidden sm:table-cell">Requested</th>
          <th class="trellis-primary">Changes</th>
          <th><span class="max-sm:sr-only">Result</span></th>
          <th class="hidden sm:table-cell">Decided by</th>
        </tr>
      </thead>
      <tbody>
        {#each shown as update (update.updateId)}
          {@const summary = summarizeUpdateChanges(update.changes)}
          {@const state = STATES[update.state] ?? {
            label: update.state,
            status: "offline",
          }}
          <tr class="align-top">
            <td class="hidden whitespace-nowrap tabular-nums sm:table-cell"
              >{formatDate(update.requestedAt)}</td
            >
            <td class="trellis-primary">
              {#if summary.length}
                <button
                  type="button"
                  class="text-left hover:underline"
                  aria-expanded={open === update.updateId}
                  onclick={() =>
                    (open = open === update.updateId ? null : update.updateId)}
                  >{summary.join(", ")}</button
                >
              {:else}<span class="text-base-content/65"
                  >New build, same access</span
                >{/if}
              <span class="block text-xs text-base-content/60 sm:hidden"
                >{formatDate(update.requestedAt)} · {decider(update)}</span
              >
              {#if open === update.updateId}
                <dl
                  class="mt-2 grid gap-x-3 gap-y-1 text-xs sm:grid-cols-[6rem_minmax(0,1fr)]"
                >
                  {#each details(update) as { label, items } (label)}
                    {#if items.length}
                      <dt class="text-base-content/60">{label}</dt>
                      <dd>
                        <ul class="space-y-0.5">
                          {#each items as item (item)}<li>{item}</li>{/each}
                        </ul>
                      </dd>
                    {/if}
                  {/each}
                </dl>
              {/if}
            </td>
            <td
              ><StatusBadge
                compact
                label={state.label}
                status={state.status}
              /></td
            >
            <td class="hidden text-base-content/75 sm:table-cell"
              >{decider(update)}</td
            >
          </tr>
        {/each}
      </tbody>
    </DataTable>
    {#if decided.length > LATEST}
      <div class="border-t border-base-300 px-5 py-3">
        <button
          type="button"
          class="btn btn-ghost btn-sm"
          onclick={() => (showAll = !showAll)}
          >{showAll ? "Show latest" : `Show all ${decided.length}`}</button
        >
      </div>
    {/if}
  {/if}
</section>
