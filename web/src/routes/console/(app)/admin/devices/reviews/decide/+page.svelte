<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { traverseAll, resolveExact } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type Review = apis.auth.DeviceUserAuthoritiesReviewsListOutput["items"][number];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("device-review-decide");
  type DecideInput = { reviewId: string; decision: "approve" | "reject"; expectedVersion: Review["version"]; idempotencyKey: string; reason: string | null };
  const mutation = new MutationController<DecideInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let reviews = $state<Review[]>([]);
  const requestedReviewId = $derived(page.url.searchParams.get("review") ?? "");
  let selectedReviewId = $state("");
  let decision = $state<"approve" | "reject" | "">("");
  let reason = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  /** Set after a successful decision so the old target leaves the list. */
  let completedReviewId = $state<string | null>(null);

  // Only pending, unexpired reviews may be decided; terminal and expired
  // reviews stay visible in their own state but are not candidates.
  const decidableReviews = $derived(
    reviews.filter((review) => review.state === "pending"),
  );
  const selectedReview = $derived(
    decidableReviews.find((review) => review.reviewId === (requestedReviewId || selectedReviewId)) ??
      null,
  );
  const requestedUnavailable = $derived(
    requestedReviewId !== "" && selectedReview === null && !loading && !error && completedReviewId !== requestedReviewId,

  );

  function reviewStatus(state: Review["state"]): "healthy" | "degraded" | "unhealthy" | "offline" {
    return state === "pending" ? "degraded" : state === "approved" ? "healthy" : state === "rejected" ? "unhealthy" : "offline";
  }

  async function load(requestedId: string) {
    const token = scope.begin();
    const previousSelection = selectedReviewId;
    loading = true;
    error = null;
    reviews = [];
    selectedReviewId = "";
    try {
      const readPage = async (request: { limit: number; cursor?: string }) => {
        const response = await trellis.deviceUserAuthoritiesReviewsList({ state: "pending", page: request }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor ?? undefined };
      };
      const result = requestedId
        ? await resolveExact<Review>(readPage, (review) => review.reviewId === requestedId)
        : await traverseAll<Review>(readPage);
      if (!scope.isCurrent(token)) return;
      if (!result.complete) { error = projectConsoleError(result.error).message; return; }
      reviews = requestedId ? ("item" in result && result.item ? [result.item] : []) : ("items" in result ? [...result.items] : []);
      if (!requestedId && reviews.some((review) => review.reviewId === previousSelection && review.state === "pending")) selectedReviewId = previousSelection;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestDecision() {
    const target = selectedReview;
    if (!target || !decision || pending || loading || error) return;
    const requestedAtCapture = requestedReviewId;
    const key = ulid();
    const intent = captureIntent<DecideInput>({
      operation: "deviceUserAuthoritiesReviewsDecide", targetId: target.reviewId, label: target.reviewId,
      expectedValue: decision === "reject" ? target.reviewId : undefined, idempotencyKey: key,
      input: { reviewId: target.reviewId, decision, expectedVersion: target.version, idempotencyKey: key, reason: reason.trim() || null },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: intent.input.decision === "reject"
        ? "Reject activation review?"
        : "Approve activation review?",
      message: intent.input.decision === "reject"
        ? "This records a rejected terminal decision for this review."
        : "This approves the administrative review. Any required companion or user-authority step may still be pending before activation completes.",
      confirmLabel: intent.input.decision === "reject" ? "Reject review" : "Approve review",
      targetLabel: "Review",
      targetName: intent.targetId,
      expectedValue: intent.expectedValue,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && requestedReviewId === requestedAtCapture && selectedReview?.reviewId === intent.targetId,

      dispatch: async ({ input }) => await trellis.deviceUserAuthoritiesReviewsDecide(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedReviewId !== requestedAtCapture || false) return;

    if (outcome.kind === "succeeded") {
      completedReviewId = intent.targetId;
      notifications.success(`Review ${intent.targetId} ${intent.input.decision === "approve" ? "approved" : "rejected"}.`, intent.input.decision === "approve" ? "Approved" : "Rejected");
      reason = "";
      decision = "";
      await load(requestedReviewId);
    } else if (outcome.kind === "unknown") {
      error = "Outcome unknown; the decision may have completed. Check the review before trying again.";
    } else error = projectConsoleError(outcome.error).message;
  }

  $effect(() => {
    const id = requestedReviewId;
    scope.setKey(`device-review-decide:${id}`);
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(id));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Decide activation review" description="Approve or reject a pending administrative review.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if requestedUnavailable}
    <Notice variant="warning">
      Review '{requestedReviewId}' is not a pending, decidable review.
    </Notice>
  {/if}

  {#if completedReviewId}
    <Notice variant="success">
      Review {completedReviewId} was decided. Any required companion or user-authority step may still be pending.
    </Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading pending reviews" /></Panel>
  {:else if error}
    <Panel><p class="text-sm">Review lookup is incomplete. <button type="button" class="btn btn-ghost btn-xs" onclick={() => void load(requestedReviewId)}>Retry</button></p></Panel>
  {:else if requestedUnavailable}
    <EmptyState
      title="Review unavailable"
      description={`No pending review matches '${requestedReviewId}'. It may already be decided or expired.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
      {/snippet}
    </EmptyState>
  {:else if decidableReviews.length === 0}
    <EmptyState title="No pending reviews" description="There are no pending activation reviews to decide." />
  {:else}
    <div class="grid gap-4 lg:grid-cols-[minmax(0,1fr)_24rem]">
      <Panel title="Decision" eyebrow="Review workflow">
        <div class="mb-4 rounded-box border border-base-300 bg-base-200/40 p-3 text-xs text-base-content/60">
          This records the administrative review decision. Final activation can still
          depend on a companion or user-authority step; retry a decision only with the
          same terminal result.
        </div>
        <form class="space-y-4" onsubmit={(event) => { event.preventDefault(); void requestDecision(); }}>
          <label class="form-control gap-1">
            <span class="label-text text-xs">Review</span>
            {#if requestedReviewId !== ""}
              <input
                class="input input-bordered input-sm font-mono"
                value={requestedReviewId}
                readonly
                aria-label="Review"
              />
            {:else}
              <select class="select select-bordered select-sm" bind:value={selectedReviewId} required>
                <option value="" disabled>Select a pending review…</option>
                {#each decidableReviews as review (review.reviewId)}
                  <option value={review.reviewId}>{review.reviewId} · {review.deploymentId}</option>
                {/each}
              </select>
            {/if}
          </label>

          <label class="form-control gap-1">
            <span class="label-text text-xs">Decision</span>
            <select class="select select-bordered select-sm" bind:value={decision} required>
              <option value="" disabled>Choose approve or reject</option>
              <option value="approve">Approve</option>
              <option value="reject">Reject</option>
            </select>
          </label>

          <label class="form-control gap-1">
            <span class="label-text text-xs">Reason</span>
            <textarea class="textarea textarea-bordered textarea-sm min-h-24" bind:value={reason} placeholder="Optional decision reason"></textarea>
          </label>

          <div class="flex justify-end">
            <button
              type="submit"
              class={["btn btn-sm", decision === "reject" ? "btn-error" : "btn-outline"]}
               disabled={pending || !selectedReview || !decision || requestedUnavailable}
            >
              {pending ? "Submitting…" : decision === "approve" ? "Approve review" : decision === "reject" ? "Reject review" : "Submit decision"}
            </button>
          </div>
        </form>
      </Panel>

      <Panel title="Review detail" eyebrow="Selected review" class="min-w-0">
         {#if selectedReview}
           {@const review = selectedReview}
          {#if review}
          <div class="space-y-3 text-sm">
            <div class="flex items-center justify-between gap-3">
              <span class="trellis-identifier">{review.reviewId}</span>
              <StatusBadge label={review.state} status={reviewStatus(review.state)} />
            </div>
            <div>
              <p class="text-[0.65rem] font-semibold uppercase tracking-wider text-base-content/50">Instance</p>
              <p class="trellis-identifier">{review.instanceId}</p>
              <p class="trellis-identifier text-base-content/60">{review.devicePrincipalId}</p>
            </div>
            <div class="grid grid-cols-2 gap-2">
              <div><span class="text-base-content/50">Deployment</span><div class="trellis-identifier">{review.deploymentId}</div></div>
              <div><span class="text-base-content/50">Requested</span><div>{formatDate(review.requestedAt)}</div></div>
              <div><span class="text-base-content/50">Expires</span><div>{formatDate(review.expiresAt)}</div></div>
              <div><span class="text-base-content/50">Version</span><div>{review.version}</div></div>
            </div>
          </div>
          {/if}
        {/if}
      </Panel>
    </div>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
