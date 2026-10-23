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
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { formatDate } from "$lib/format";
  import { capabilityArea } from "$lib/console/capability_areas.ts";

  type Review = apis.auth.AdministrationReviewsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("administration-reviews");
  let loading = $state(true);
  let error = $state<string | null>(null);
  let decisionError = $state<string | null>(null);
  let pending = $state<string | null>(null);
  let reviews = $state.raw<Review[]>([]);
  // Capability titles name what an app asked for; IDs are the fallback.
  let titles = $state.raw<Map<string, string>>(new Map());
  const ordered = $derived(
    [...reviews].sort(
      (left, right) =>
        Number(right.state === "pending") - Number(left.state === "pending") ||
        right.requestedAt.localeCompare(left.requestedAt),
    ),
  );

  function asked(capabilityId: string): string {
    return (
      titles.get(capabilityId) ??
      `${capabilityArea(capabilityId).title}: ${capabilityId.split(".").at(-1)}`
    );
  }

  const STATES: Record<
    string,
    { label: string; tone: "healthy" | "degraded" | "offline" }
  > = {
    pending: { label: "Waiting", tone: "degraded" },
    approved: { label: "Approved", tone: "healthy" },
    declined: { label: "Rejected", tone: "offline" },
  };

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
    void traverseAll(async (page) => {
      const response = await trellis.capabilitiesList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    }).then((result) => {
      titles = new Map(
        result.items.map((item) => [item.identity.capabilityId, item.title]),
      );
    });
    return () => scope.dispose();
  });
</script>

<section class="space-y-5">
  <PageToolbar
    title="Admin approvals"
    description="When someone lets an app manage this Trellis, another administrator approves it here."
    >{#snippet actions()}<button
        class="btn btn-outline btn-sm"
        disabled={loading}
        onclick={() => void load()}>Refresh</button
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="warning"
      >Some requests couldn't be loaded: {error}</Notice
    >{/if}
  {#if decisionError}<Notice variant="warning"
      >Your decision wasn't saved: {decisionError}</Notice
    >{/if}
  {#if loading}<LoadingState
      label="Loading requests"
    />{:else if !reviews.length}<EmptyState
      title="Nothing to approve"
      description="Requests appear here when someone lets an app manage this Trellis. Nobody can approve their own request."
    />{:else}
    <ul class="trellis-section divide-y divide-base-300">
      {#each ordered as review (review.reviewId)}
        {@const state = STATES[review.state] ?? {
          label: review.state,
          tone: "offline" as const,
        }}
        <li class="flex flex-wrap items-center justify-between gap-4 px-5 py-4">
          <div class="min-w-0">
            <p class="font-medium">
              {review.clientName} wants to {asked(
                review.capabilityId,
              ).toLowerCase()}
            </p>
            <p class="text-sm text-base-content/65">
              Allowed by {review.principalTitle} · {formatDate(
                review.requestedAt,
              )}
            </p>
          </div>
          <div class="flex items-center gap-3">
            <StatusBadge label={state.label} status={state.tone} />
            {#if review.state === "pending"}
              <button
                class="btn btn-primary btn-sm"
                disabled={pending !== null ||
                  !canPerform(
                    authority.authority,
                    "administrationReviewsDecide",
                  )}
                onclick={() => void decide(review, "approved")}>Approve</button
              >
              <button
                class="btn btn-ghost btn-sm text-error"
                disabled={pending !== null ||
                  !canPerform(
                    authority.authority,
                    "administrationReviewsDecide",
                  )}
                onclick={() => void decide(review, "declined")}>Reject</button
              >
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>
