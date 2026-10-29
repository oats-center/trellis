<script lang="ts">
  import { onMount } from "svelte";
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { isUncertainError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { consoleUrl } from "$lib/console_paths";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Pager from "$lib/components/Pager.svelte";
  import { formatDate, humanizeName } from "$lib/format";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";

  type Api = apis.auth.ApisListOutput["items"][number];
  const trellis = getTrellis();

  function apiName(apiId: string): string {
    return humanizeName(apiId.split("@")[0] ?? apiId);
  }

  // `trellis.auth@v1` belongs to the `trellis` namespace.
  function namespaceOf(apiId: string): string {
    const name = apiId.split("@")[0] ?? apiId;
    return name.includes(".") ? name.slice(0, name.lastIndexOf(".")) : name;
  }

  function apiVersion(apiId: string): string {
    return apiId.split("@")[1] ?? "";
  }

  type Review = apis.auth.ApisReviewOutput["review"];

  /** Capability titles come from the accepted catalog; the ID is the fallback. */
  function capabilityTitle(capabilityId: string): string {
    for (const api of catalog) {
      const found = api.capabilities.find(
        (capability) => capability.identity.capabilityId === capabilityId,
      );
      if (found) return found.title;
    }
    return humanizeName(capabilityId.split("::").at(-1) ?? capabilityId);
  }

  const ACTION_KINDS: Record<string, string> = {
    rpc: "Request",
    operation: "Operation",
    event: "Event",
    live: "Live updates",
    transfer: "File transfer",
  };

  /** Each changed action once, whatever directions it covers. */
  function changedActions(value: Review): { name: string; kind: string }[] {
    const seen = new Map<string, { name: string; kind: string }>();
    for (const action of value.actionChanges) {
      seen.set(`${action.kind}:${action.name}`, {
        name: humanizeName(action.name),
        kind: ACTION_KINDS[action.kind] ?? humanizeName(action.kind),
      });
    }
    return [...seen.values()];
  }

  function plural(count: number, word: string): string {
    return `${count} ${word}${count === 1 ? "" : "s"}`;
  }
  const authority = getConsoleAuthority();
  const scope = new RequestScope("api-catalog");
  const reviewScope = new RequestScope("api-review");
  let catalog = $state.raw<apis.auth.ApisListOutput["items"]>([]);
  let search = $state("");
  const namespaces = $derived.by(() => {
    const byId = new Map<string, Api[]>();
    const term = search.trim().toLowerCase();
    for (const api of catalog.filter((item) =>
      `${item.title} ${item.description} ${item.apiId}`
        .toLowerCase()
        .includes(term),
    )) {
      const id = namespaceOf(api.apiId);
      byId.set(id, [...(byId.get(id) ?? []), api]);
    }
    return [...byId]
      .map(([id, apis]) => ({ id, apis }))
      .sort((a, b) => a.id.localeCompare(b.id));
  });
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
          ? "We couldn't confirm the change. Check the API before trying again."
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

<section class="space-y-5">
  <PageToolbar
    title="APIs"
    description="What's installed, what each API offers, and how to call it."
  />
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if result}<Notice variant="success"
      >{apiName(result.api.apiId)} is updated.</Notice
    >{/if}

  <input
    class="input w-full max-w-sm"
    type="search"
    placeholder="Search APIs"
    aria-label="Search APIs"
    bind:value={search}
  />
  <div class="trellis-section min-w-0">
    {#if loading}<LoadingState label="Loading APIs" />{:else if !catalog.length}
      <EmptyState class="m-4" title="No APIs installed" />
    {:else if !namespaces.length}
      <EmptyState class="m-4" title="No APIs match" />
    {:else}
      {#each namespaces as namespace (namespace.id)}
        <h3
          class="border-b border-base-300 bg-base-200/60 px-5 py-2 text-sm font-semibold"
        >
          {humanizeName(namespace.id)}
          <span class="font-normal text-base-content/55"
            >· {namespace.apis.length}
            {namespace.apis.length === 1 ? "API" : "APIs"}</span
          >
        </h3>
        <ul class="divide-y divide-base-300">
          {#each namespace.apis as api (api.apiId)}
            <li>
              <a
                class="flex w-full items-center gap-3 px-5 py-3 text-left hover:bg-base-200/60"
                href={consoleUrl("/admin/apis/view", {
                  query: { apiId: api.apiId },
                })}
              >
                <span
                  class="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-base-200 text-base-content/70"
                  ><Icon name="braces" size={16} /></span
                >
                <span class="min-w-0 flex-1">
                  <span class="block font-medium">{api.title}</span>
                  <span class="block truncate text-sm text-base-content/65"
                    >{api.description}</span
                  >
                </span>
                <span class="hidden text-xs text-base-content/55 sm:block"
                  >{api.capabilities.filter(
                    (item) => item.kind === "capability",
                  ).length} permissions · installed {formatDate(
                    api.acceptedAt,
                  )}</span
                >
                <span class="badge badge-sm badge-neutral"
                  >{api.version ?? apiVersion(api.apiId)}</span
                >
              </a>
            </li>
          {/each}
        </ul>
      {/each}
      {#if history.length || next}
        <Pager
          summary={`Showing ${catalog.length} APIs on this page`}
          canPrevious={history.length > 0}
          canNext={!!next}
          busy={loading || busy}
          onPrevious={() => {
            cursor = history.at(-1);
            history = history.slice(0, -1);
            void load();
          }}
          onNext={() => {
            history = [...history, cursor];
            cursor = next;
            void load();
          }}
        />
      {/if}
    {/if}
  </div>

  {#if canPerform(authority.authority, "apisReview")}
    <details class="trellis-section p-5">
      <summary class="cursor-pointer font-medium"
        >Review an API update manually</summary
      >
      <p class="mt-2 text-sm text-base-content/65">
        Updates are usually installed with <code>trellis apis</code>. Paste a
        review request here to check one in the browser.
      </p>
      <form
        class="mt-3 space-y-3"
        onsubmit={(event) => {
          event.preventDefault();
          void reviewCandidate();
        }}
      >
        <label class="trellis-field"
          ><span class="trellis-field-label">Review request</span><textarea
            class="textarea w-full font-mono text-xs"
            rows="6"
            bind:value={request}
            required
            disabled={busy}
          ></textarea></label
        >
        <button class="btn btn-outline" disabled={busy}>Check update</button>
      </form>
      {#if review}
        <div class="mt-5 border-t border-base-300 pt-4">
          <p class="font-medium">
            {apiName(review.apiId)}:
            {review.compatible
              ? "safe to install"
              : "this update breaks existing apps and services"}
          </p>
          <div class="mt-3 space-y-3 text-sm">
            {#if review.breakage.length > 0}
              <div>
                <h3 class="font-medium">What breaks</h3>
                <ul class="mt-1 list-disc space-y-0.5 pl-5">
                  {#each review.breakage as item, index (index)}<li>
                      {item}
                    </li>{/each}
                </ul>
              </div>
            {/if}
            {#if review.capabilityChanges.length > 0}
              <div>
                <h3 class="font-medium">Permission changes</h3>
                <ul class="mt-1 space-y-0.5">
                  {#each review.capabilityChanges as change (change.capabilityId)}
                    <li>
                      {capabilityTitle(change.capabilityId)}
                      <span class="text-base-content/65"
                        >· {change.description === "Removed"
                          ? "removed"
                          : "meaning changed; people will be asked again"}</span
                      >
                    </li>
                  {/each}
                </ul>
              </div>
            {/if}
            {#if review.actionChanges.length > 0}
              <div>
                <h3 class="font-medium">Changed actions</h3>
                <ul class="mt-1 space-y-0.5">
                  {#each changedActions(review) as action (`${action.kind}:${action.name}`)}
                    <li>
                      {action.name}
                      <span class="text-base-content/65">· {action.kind}</span>
                    </li>
                  {/each}
                </ul>
              </div>
            {/if}
            {#if review.affectedRoleIds.length > 0 || review.affectedDeploymentIds.length > 0}
              <p class="text-base-content/70">
                Affects {plural(review.affectedRoleIds.length, "role")} and
                {plural(review.affectedDeploymentIds.length, "service")}.
              </p>
            {/if}
            {#if review.breakage.length === 0 && review.capabilityChanges.length === 0 && review.actionChanges.length === 0}
              <p class="text-base-content/70">
                No changes to existing behavior.
              </p>
            {/if}
          </div>
          <fieldset class="mt-3 space-y-3" disabled={busy}>
            <details class="text-sm">
              <summary class="cursor-pointer text-base-content/70"
                >Role and sign-in rule changes</summary
              >
              <label class="trellis-field mt-2"
                ><span class="trellis-field-label">Role edits</span><textarea
                  class="textarea w-full font-mono text-xs"
                  bind:value={roleEdits}
                ></textarea></label
              >
              <label class="trellis-field mt-2"
                ><span class="trellis-field-label">Sign-in rule edits</span
                ><textarea
                  class="textarea w-full font-mono text-xs"
                  bind:value={mappingEdits}
                ></textarea></label
              >
            </details>
            {#if review.compatible && canPerform(authority.authority, "apisAccept")}<button
                class="btn btn-primary"
                onclick={() => void accept(false)}>Install update</button
              >{/if}
            {#if !review.compatible && canPerform(authority.authority, "apisForceReplace")}
              <label class="flex items-center gap-2"
                ><input
                  type="checkbox"
                  class="checkbox checkbox-sm"
                  bind:checked={acknowledgeBreakage}
                />I understand apps and services using this API will stop
                working until updated.</label
              >
              <label class="flex items-center gap-2"
                ><input
                  type="checkbox"
                  class="checkbox checkbox-sm"
                  bind:checked={acknowledgeConsentImpact}
                />I understand people may need to approve access again.</label
              >
              <button
                class="btn btn-error"
                disabled={!acknowledgeBreakage || !acknowledgeConsentImpact}
                onclick={() => void accept(true)}>Replace anyway</button
              >
            {/if}
          </fieldset>
        </div>
      {/if}
    </details>
  {/if}
</section>
