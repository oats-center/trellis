<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { formatDate } from "$lib/format";

  type Review = apis.auth.AdministrationReviewsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("administration-reviews");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let decisionError = $state<string | null>(null);
  let pending = $state<string | null>(null);
  let reviews = $state.raw<Review[]>([]);

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    const result = await traverseAll(async (page) => {
      const response = await trellis
        .administrationReviewsList({ page })
        .orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    if (!scope.settle(token)) return;
    reviews = [...result.items];
    error = result.complete ? null : projectConsoleError(result.error).message;
    loading = false;
  }

  async function decide(
    review: Review,
    decision: "approved" | "declined",
  ): Promise<void> {
    if (
      pending ||
      !canPerform(authority.authority, "administrationReviewsDecide")
    )
      return;
    pending = review.reviewId;
    decisionError = null;
    const result = await trellis.administrationReviewsDecide({
      reviewId: review.reviewId,
      decision,
      expectedRevision: review.revision,
      idempotencyKey: ulid(),
    });
    if (result.isErr())
      decisionError = projectConsoleError(result.error).message;
    pending = null;
    await load();
  }

  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="Administration reviews"
    description="Apps that a user allowed to manage this Trellis wait here until another administrator approves. Nobody can approve their own request."
    >{#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load()}>Refresh</button
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="warning">Review list incomplete: {error}</Notice
    >{/if}
  {#if decisionError}<Notice variant="warning"
      >The decision was not recorded: {decisionError}</Notice
    >{/if}
  {#if loading}<Panel><LoadingState label="Loading reviews" /></Panel
    >{:else if !reviews.length}<EmptyState
      title="No administration reviews"
      description="A review opens when a user lets an app manage this Trellis."
    />{:else}
    <Panel title="Reviews" eyebrow={`${reviews.length} loaded records`}
      ><DataTable
        ><thead
          ><tr
            ><th>User</th><th>App</th><th>Capability</th><th>Requested</th><th
              >State</th
            ><th>Actions</th></tr
          ></thead
        ><tbody>
          {#each reviews as review (review.reviewId)}<tr
              ><td
                >{review.principalTitle}
                <div class="trellis-metadata">{review.principalId}</div></td
              ><td
                >{review.clientName}
                <div class="trellis-metadata">{review.clientId}</div></td
              ><td class="trellis-identifier">{review.capabilityId}</td><td
                >{formatDate(review.requestedAt)}</td
              ><td
                ><span
                  class="badge badge-xs {review.state === 'approved'
                    ? 'badge-success'
                    : review.state === 'pending'
                      ? 'badge-warning'
                      : 'badge-neutral'}">{review.state}</span
                ></td
              ><td
                >{#if review.state === "pending"}<button
                    class="btn btn-primary btn-xs"
                    disabled={pending !== null ||
                      !canPerform(
                        authority.authority,
                        "administrationReviewsDecide",
                      )}
                    onclick={() => void decide(review, "approved")}
                    >Approve</button
                  >
                  <button
                    class="btn btn-ghost btn-xs"
                    disabled={pending !== null ||
                      !canPerform(
                        authority.authority,
                        "administrationReviewsDecide",
                      )}
                    onclick={() => void decide(review, "declined")}
                    >Reject</button
                  >{/if}</td
              ></tr
            >{/each}
        </tbody></DataTable
      ></Panel
    >
  {/if}
</section>
