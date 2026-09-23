<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { classifyTargetFailure } from "$lib/console/target.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { formatDate } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type GrantBinding = apis.auth.GrantsListOutput["items"][number];

  const trellis = getTrellis();
  const notifications = getNotifications();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-grant-revoke");
  type RevokeInput = Parameters<typeof trellis.grantsRevoke>[0];
  const mutation = new MutationController<RevokeInput, unknown>((state) => pending = state.busy);

  let loading = $state(true);
  let pending = $state(false);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let identityGrants = $state.raw<GrantBinding[]>([]);
  let selectedKey = $state("");
  /** Set after a successful revoke so the tombstone is not auto-replaced. */
  let completedKey = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();

  // Canonical tuple parameters, never a colon-concatenated opaque key.
  const requestedOwnerKind = $derived(page.url.searchParams.get("ownerKind") ?? "");
  const requestedOwnerId = $derived(page.url.searchParams.get("ownerId") ?? "");
  const requestedParticipantId = $derived(
    page.url.searchParams.get("participantId") ?? "",
  );
  const pinnedTuple = $derived(
    page.url.searchParams.has("ownerKind") || page.url.searchParams.has("ownerId") || page.url.searchParams.has("participantId"),

  );

  const selectedGrant = $derived(
    identityGrants.find((entry) => grantKey(entry) === selectedKey) ?? null,
  );
  const requestedGrant = $derived(
    pinnedTuple
      ? identityGrants.find((entry) =>
        entry.ownerKind === requestedOwnerKind && entry.ownerId === requestedOwnerId &&

        entry.participantId === requestedParticipantId
      ) ?? null
      : null,
  );
  /** A retained revoked binding stays visible but is not revocable again. */
  const targetIsActive = $derived(
    (selectedGrant ?? requestedGrant)?.state === "active",
  );
  const requestedUnavailable = $derived(
    pinnedTuple && requestedGrant === null && completedKey === null && !error,
  );

  function grantKey(entry: GrantBinding): string {
    return JSON.stringify([entry.ownerKind, entry.ownerId, entry.participantId]);
  }

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  async function load(ownerKind: string, ownerId: string, participantId: string, pinned: boolean): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    identityGrants = [];
    selectedKey = "";
    try {
      if (pinned) {
        if (ownerKind !== "user" || !ownerId || !participantId) return;
        const response = await trellis.grantsGet({ ownerKind: "user", ownerId, participantId }).take();
        if (!scope.isCurrent(token)) return;
        if (isErr(response)) {
          if (classifyTargetFailure(response) !== "not-found") error = failureFrom(response);
          return;
        }
        const binding = response.binding;
        if (binding && binding.ownerKind === ownerKind && binding.ownerId === ownerId && binding.participantId === participantId) {
          identityGrants = [binding];
          selectedKey = grantKey(binding);
        }
        return;
      }

      const result = await traverseAll<GrantBinding>(async (pageRequest) => {
        const response = await trellis.grantsList({
          ownerKind: "user",
          page: catalogPage(pageRequest.cursor),
        }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      if (!result.complete) {
        error = failureFrom(result.error);
        return;
      }
      identityGrants = [...result.items];
      // No deep link: keep an explicit selection; never auto-pick a row.
      if (
        selectedKey !== "" && !identityGrants.some((entry) => grantKey(entry) === selectedKey)

      ) {
        selectedKey = "";
      }
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function requestRevokeGrant(): Promise<void> {
    const target = pinnedTuple ? requestedGrant : selectedGrant;
    if (!target || target.state !== "active" || completedKey === grantKey(target) || loading || error || pending) return;
    const key = ulid();
    const requestedAtCapture = [requestedOwnerKind, requestedOwnerId, requestedParticipantId, pinnedTuple];
    const intent = captureIntent({
      operation: "grantsRevoke", targetId: grantKey(target), label: `${target.ownerId} / ${target.participantId}`,
      expectedValue: target.participantId, idempotencyKey: key,
      input: { ownerId: target.ownerId, ownerKind: "user", participantId: target.participantId, expectedRevision: target.revision, idempotencyKey: key },
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Revoke user-owned grant?",
      message: "This revokes the exact binding for this owner and participant.",
      confirmLabel: "Revoke grant",
      targetLabel: "Owner / participant",
      targetName: intent.label,
      expectedValue: intent.expectedValue,
      details: `Owner kind: ${target.ownerKind} · revision ${target.revision}`,
    });
    if (!confirmed) { mutation.cancel(); return; }
    const outcome = await mutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && requestedAtCapture[0] === requestedOwnerKind && requestedAtCapture[1] === requestedOwnerId &&

        requestedAtCapture[2] === requestedParticipantId && requestedAtCapture[3] === pinnedTuple && (pinnedTuple ? requestedGrant : selectedGrant)?.revision === intent.input.expectedRevision &&

        (pinnedTuple ? requestedGrant : selectedGrant)?.ownerId === intent.input.ownerId && (pinnedTuple ? requestedGrant : selectedGrant)?.participantId === intent.input.participantId && !loading && !error,

      dispatch: async ({ input }) => await trellis.grantsRevoke(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey || requestedAtCapture[0] !== requestedOwnerKind || requestedAtCapture[1] !== requestedOwnerId ||

      requestedAtCapture[2] !== requestedParticipantId || requestedAtCapture[3] !== pinnedTuple) return;
    if (outcome.kind === "succeeded") {
      completedKey = intent.targetId;
      notifications.success("User-owned grant revoked.", "Revoked");
      await load(requestedOwnerKind, requestedOwnerId, requestedParticipantId, pinnedTuple);
    } else if (outcome.kind === "unknown") {
      error = { message: "Outcome unknown; the revoke may have completed. Check the grant before another operation." };
    } else {
      error = failureFrom(outcome.error);
    }
  }

  $effect(() => {
    const ownerKind = requestedOwnerKind;
    const ownerId = requestedOwnerId;
    const participantId = requestedParticipantId;
    const pinned = pinnedTuple;
    scope.setKey(JSON.stringify([ownerKind, ownerId, participantId, pinned]));
    identityGrants = [];
    selectedKey = "";
    completedKey = null;
    untrack(() => confirmationModal?.cancel());
    mutation.cancel();
    untrack(() => void load(ownerKind, ownerId, participantId, pinned));
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar title="Revoke user-owned grant" description="Confirm and revoke one exact user-owned grant binding.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/apps")}>Back to user grants</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
      <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={() => void load(requestedOwnerKind, requestedOwnerId, requestedParticipantId, pinnedTuple)}>Retry</button>
    </Notice>
  {/if}

  {#if completedKey}
    <Notice variant="success">
      The selected grant is revoked. A retained revoked binding is not revocable again.
    </Notice>
  {/if}

  {#if loading && !error}
    <Panel><LoadingState label="Loading user-owned grants" /></Panel>
  {:else if error}
    <Panel><p class="text-sm text-base-content/60">Grant lookup is incomplete. Retry before selecting a target.</p></Panel>
  {:else if requestedUnavailable || identityGrants.length === 0}
    <EmptyState
      title="Grant unavailable"
      description={pinnedTuple
        ? `No user-owned grant matches owner '${requestedOwnerId}' and participant '${requestedParticipantId}'. It may already be removed, or the link may be stale.`
        : "No user-owned grant bindings are available to revoke."}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/apps")}>Back to user grants</a>
      {/snippet}
    </EmptyState>
  {:else}
    <Panel title="Confirm revoke" eyebrow="Workflow">
      <div class="space-y-4">
        <label class="form-control gap-1">
          <span class="label-text text-xs">User-owned grant</span>
          {#if pinnedTuple}
            <input
              class="input input-bordered input-sm font-mono"
              value={`${requestedOwnerId} · ${requestedParticipantId}`}
              readonly
              aria-label="Grant"
            />
          {:else}
            <select class="select select-bordered select-sm" bind:value={selectedKey} required>
              <option value="" disabled>Select a grant…</option>
              {#each identityGrants as entry (grantKey(entry))}
                <option value={grantKey(entry)}>{entry.ownerId} · {entry.participantId} · {entry.state}</option>
              {/each}
            </select>
          {/if}
        </label>

        {#if selectedGrant ?? requestedGrant}
          {@const target = selectedGrant ?? requestedGrant}
          {#if target}
            <div class="rounded-box border border-base-300 p-3 text-sm">
              <div class="trellis-identifier font-medium">{target.participantId}</div>
              <div class="text-base-content/60">Owner: <span class="trellis-identifier">{target.ownerId}</span></div>
              <div>
                State:
                <span class="badge badge-sm {target.state === "active" ? "badge-success" : "badge-neutral"}">{target.state}</span>
              </div>
              <div class="trellis-identifier text-base-content/60">installed revision {target.installedRevision}</div>
              <div class="trellis-identifier text-base-content/60">grant revision {target.revision}</div>
              <div class="text-xs text-base-content/60">Updated {formatDate(target.updatedAt)}</div>
            </div>
          {/if}
        {/if}

        <div class="flex flex-wrap gap-2">
          <button
            class="btn btn-error btn-sm"
            onclick={requestRevokeGrant}
            disabled={!targetIsActive || (selectedGrant !== null && completedKey === grantKey(selectedGrant)) || pending || loading || !!error || requestedUnavailable}
          >
            {pending ? "Revoking..." : "Revoke grant"}
          </button>
          <a class="btn btn-ghost btn-sm" href={resolve("/admin/apps")}>Cancel</a>
        </div>
      </div>
    </Panel>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
