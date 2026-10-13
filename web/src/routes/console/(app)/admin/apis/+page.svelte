<script lang="ts">
  import { onMount } from "svelte";
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { isUncertainError } from "$lib/console/mutation.ts";
  import {
    displayJson,
    projectConsoleError,
  } from "$lib/console/display_value.ts";
  import DataTable from "$lib/components/DataTable.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("api-catalog");
  const reviewScope = new RequestScope("api-review");
  let catalog = $state.raw<apis.auth.ApisListOutput["items"]>([]);
  let detail = $state.raw<apis.auth.ApisGetOutput | null>(null);
  let review = $state.raw<apis.auth.ApisReviewOutput["review"] | null>(null);
  let result = $state.raw<apis.auth.ApisAcceptOutput | null>(null);
  let cursor = $state<string | undefined>();
  let next = $state<string | undefined>();
  let history = $state<(string | undefined)[]>([]);
  let loading = $state(true);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let request = $state("");
  let roleEdits = $state("[]");
  let mappingEdits = $state("[]");
  let acknowledgeBreakage = $state(false);
  let acknowledgeConsentImpact = $state(false);

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    try {
      const response = await trellis
        .apisList({ page: { cursor, limit: 50 } })
        .orThrow();
      if (!scope.isCurrent(token)) return;
      catalog = response.items;
      next = response.page.nextCursor;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function inspect(apiId: string): Promise<void> {
    const token = reviewScope.begin();
    detail = null;
    try {
      const response = await trellis.apisGet({ apiId }).orThrow();
      if (reviewScope.settle(token)) detail = response;
    } catch (cause) {
      if (reviewScope.settle(token)) error = projectConsoleError(cause).message;
    }
  }

  async function reviewCandidate(): Promise<void> {
    if (busy) return;
    const token = reviewScope.begin();
    busy = true;
    error = null;
    review = null;
    result = null;
    acknowledgeBreakage = false;
    acknowledgeConsentImpact = false;
    try {
      const input = apis.auth.API.actions["rpc:Apis.Review"].input.decode(
        JSON.parse(request),
      );
      const response = await trellis.apisReview(input).orThrow();
      if (reviewScope.isCurrent(token)) review = response.review;
    } catch (cause) {
      if (reviewScope.isCurrent(token))
        error = projectConsoleError(cause).message;
    } finally {
      if (reviewScope.settle(token)) busy = false;
    }
  }

  async function accept(force: boolean): Promise<void> {
    if (!review || busy) return;
    const current = review;
    const token = reviewScope.begin();
    busy = true;
    error = null;
    try {
      const input = {
        reviewId: current.reviewId,
        canonicalDigest: current.canonicalDigest,
        expectedGeneration: (current.currentGeneration ?? 0n).toString(),
        expectedAcceptedRevision: (
          current.currentAcceptedRevision ?? 0n
        ).toString(),
        roleEdits: JSON.parse(roleEdits),
        mappingEdits: JSON.parse(mappingEdits),
        idempotencyKey: ulid(),
      };
      const response = force
        ? await trellis
            .apisForceReplace(
              apis.auth.API.actions["rpc:Apis.ForceReplace"].input.decode({
                ...input,
                acknowledgeBreakage,
                acknowledgeConsentImpact,
              }),
            )
            .orThrow()
        : await trellis
            .apisAccept(
              apis.auth.API.actions["rpc:Apis.Accept"].input.decode(input),
            )
            .orThrow();
      if (!reviewScope.isCurrent(token)) return;
      result = response;
      review = null;
      await load();
    } catch (cause) {
      if (reviewScope.isCurrent(token))
        error = isUncertainError(cause)
          ? "Outcome unknown. Inspect the accepted API before another review."
          : projectConsoleError(cause).message;
    } finally {
      if (reviewScope.settle(token)) busy = false;
    }
  }

  onMount(() => {
    void load();
    return () => {
      scope.dispose();
      reviewScope.dispose();
    };
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title="API catalog"
    description="Review exact package evidence before changing the active accepted definition."
  >
    {#snippet actions()}<button
        class="btn btn-ghost btn-sm"
        disabled={loading || busy}
        onclick={() => void load()}>Refresh</button
      >{/snippet}
  </PageToolbar>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if result}<Notice variant="success"
      >Accepted {result.api.apiId}, generation {result.api.generation}, revision {result
        .api.acceptedRevision}.</Notice
    >{/if}
  <div class="grid gap-4 lg:grid-cols-2">
    <Panel title="Accepted APIs" eyebrow={`${catalog.length} on this page`}>
      {#if loading}<LoadingState label="Loading APIs" />{:else}<DataTable
          ><thead
            ><tr><th>API</th><th>Generation</th><th>Revision</th></tr></thead
          ><tbody>
            {#each catalog as api (api.apiId)}<tr
                ><td
                  ><button
                    class="btn btn-ghost btn-xs"
                    disabled={busy}
                    onclick={() => void inspect(api.apiId)}>{api.apiId}</button
                  ></td
                ><td>{api.generation}</td><td>{api.acceptedRevision}</td></tr
              >{:else}<tr
                ><td colspan="3">No accepted APIs on this page.</td></tr
              >{/each}
          </tbody></DataTable
        >{/if}
      <div class="flex gap-2 p-3">
        <button
          class="btn btn-ghost btn-xs"
          disabled={loading || busy || !history.length}
          onclick={() => {
            cursor = history.at(-1);
            history = history.slice(0, -1);
            void load();
          }}>Previous</button
        ><button
          class="btn btn-ghost btn-xs"
          disabled={loading || busy || !next}
          onclick={() => {
            history = [...history, cursor];
            cursor = next;
            void load();
          }}>Next</button
        >
      </div>
    </Panel>
    <Panel title="Accepted definition">
      {#if detail}<pre
          class="overflow-auto text-xs">{detail.canonicalIdl}</pre>{:else}<p
          class="trellis-metadata"
        >
          Select an API to inspect its accepted IDL and capabilities.
        </p>{/if}
    </Panel>
  </div>
  {#if canPerform(authority.authority, "apisReview")}<Panel
      title="Review candidate"
    >
      <form
        class="space-y-3"
        onsubmit={(event) => {
          event.preventDefault();
          void reviewCandidate();
        }}
      >
        <label class="form-control"
          ><span class="trellis-field-label">Review request JSON</span><textarea
            class="textarea textarea-bordered font-mono text-xs"
            rows="6"
            bind:value={request}
            required
            disabled={busy}
          ></textarea><span class="trellis-field-help"
            >AuthApiReviewRequest with apiId and packageEvidence from the
            compiled native-IDL package.</span
          ></label
        >
        <button class="btn btn-outline btn-sm" disabled={busy}
          >Review candidate</button
        >
      </form>
    </Panel>{/if}
  {#if review}<Panel
      title="Review decision"
      eyebrow={review.compatible ? "Compatible" : "Incompatible"}
    >
      <p class="trellis-identifier">
        {review.apiId} · {review.canonicalDigest}
      </p>
      <pre class="my-3 overflow-auto text-xs">{displayJson(review)}</pre>
      <fieldset class="space-y-3" disabled={busy}>
        <label class="form-control"
          ><span class="trellis-field-label">Reviewed role edits (JSON)</span
          ><textarea
            class="textarea textarea-bordered font-mono text-xs"
            bind:value={roleEdits}
          ></textarea></label
        >
        <label class="form-control"
          ><span class="trellis-field-label">Reviewed mapping edits (JSON)</span
          ><textarea
            class="textarea textarea-bordered font-mono text-xs"
            bind:value={mappingEdits}
          ></textarea></label
        >
        {#if review.compatible && canPerform(authority.authority, "apisAccept")}<button
            class="btn btn-outline btn-sm"
            onclick={() => void accept(false)}>Accept compatible API</button
          >{/if}
        {#if !review.compatible && canPerform(authority.authority, "apisForceReplace")}
          <label class="flex items-center gap-2"
            ><input
              type="checkbox"
              class="checkbox checkbox-sm"
              bind:checked={acknowledgeBreakage}
            />I acknowledge breaking existing clients and providers.</label
          >
          <label class="flex items-center gap-2"
            ><input
              type="checkbox"
              class="checkbox checkbox-sm"
              bind:checked={acknowledgeConsentImpact}
            />I acknowledge the changes to consent meaning.</label
          >
          <button
            class="btn btn-outline btn-error btn-sm"
            disabled={!acknowledgeBreakage || !acknowledgeConsentImpact}
            onclick={() => void accept(true)}>Force replacement</button
          >
        {/if}
      </fieldset>
    </Panel>{/if}
</section>
