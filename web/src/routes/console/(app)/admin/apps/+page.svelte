<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { formatDate } from "$lib/format";

  type Delegation = apis.auth.DelegationsListOutput["items"][number];
  const trellis = getTrellis();
  const scope = new RequestScope("app-delegations");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let delegations = $state.raw<Delegation[]>([]);
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
  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="App delegations"
    description="Inspect consent choices bound to exact OAuth client identities and browser origins."
    >{#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load()}>Refresh</button
      ><a class="btn btn-outline btn-sm" href={resolve("/admin/apps/revoke")}
        >Revoke delegation</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="warning"
      >Delegation catalog incomplete: {error}</Notice
    >{/if}
  {#if loading}<Panel><LoadingState label="Loading delegations" /></Panel
    >{:else if !delegations.length}<EmptyState
      title="No app delegations returned"
      description="OAuth consent creates client-bound delegations; these are not participant GrantSets."
    />{:else}
    <Panel
      title="App delegations"
      eyebrow={`${delegations.length} loaded records`}
      ><DataTable
        ><thead
          ><tr
            ><th>Delegation / Principal</th><th>Client binding</th><th
              >Capability choices</th
            ><th>Revision</th><th>Actions</th></tr
          ></thead
        ><tbody>
          {#each delegations as entry (entry.delegationId)}<tr
              ><td class="trellis-identifier"
                >{entry.delegationId}
                <div class="trellis-metadata">{entry.principalId}</div></td
              ><td class="trellis-identifier"
                >{entry.binding.clientId}
                <div class="trellis-metadata">
                  {entry.binding.kind}{#if entry.binding.origin}
                    · {entry.binding.origin}{/if}
                </div>
                {#if entry.binding.durableDpopJkt}<div
                    class="trellis-metadata break-all"
                  >
                    DPoP {entry.binding.durableDpopJkt}
                  </div>{/if}</td
              ><td
                >{#each entry.choices as choice (`${choice.capability.capabilityId}:${choice.capability.identityGeneration}`)}<div
                    class="mb-2"
                  >
                    <span class="trellis-identifier"
                      >{choice.capability.capabilityId}</span
                    ><span
                      class="badge badge-xs ml-2 {choice.decision === 'approved'
                        ? 'badge-success'
                        : 'badge-neutral'}">{choice.decision}</span
                    >
                    <div class="trellis-metadata">
                      Identity generation {choice.capability.identityGeneration} ·
                      Consent revision {choice.consentRevision}{#if choice.expiresAt}
                        · Expires {formatDate(choice.expiresAt)}{/if}
                    </div>
                  </div>{:else}No capability consent choices.{/each}</td
              ><td>{entry.revision}</td><td
                ><a
                  class="btn btn-ghost btn-xs"
                  href={resolve(
                    `/admin/apps/revoke?delegationId=${entry.delegationId}`,
                  )}>Revoke</a
                ></td
              ></tr
            >{/each}
        </tbody></DataTable
      ></Panel
    >
  {/if}
</section>
