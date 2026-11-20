<script lang="ts">
  import { apis } from "$lib/trellis-apis";
  import { onMount, untrack } from "svelte";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { apiLabel } from "$lib/console/plain_labels.ts";
  import { describeUpdateChanges } from "$lib/console/service_update_changes.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import Notice from "$lib/components/Notice.svelte";

  type Update = apis.auth.ServiceUpdatesGetOutput["update"];

  // The update waiting for a decision, or the one a review link opened. The
  // deployment's updates and the capability titles they name are bound out
  // for the page's update history.
  type Props = {
    deploymentId: string;
    /** The update a review link opened, shown even once it is decided. */
    focus?: string;
    /** Called after an approval installs the update. */
    onapplied?: () => void;
    /** Every update of the deployment, newest first, once loaded. */
    updates?: Update[];
    /** Capability titles by capability ID, for describing changes. */
    titles?: Map<string, string>;
  };
  let {
    deploymentId,
    focus,
    onapplied,
    updates = $bindable([]),
    titles = $bindable(new Map()),
  }: Props = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("service-updates");
  // A failed read of the updates; the next read clears it.
  let loadError = $state<string | null>(null);
  // The outcome of the administrator's last decision. Rereads leave it; it
  // lasts until they decide again or the page shows another target.
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  // An approval that installed the update but could not finish setting up its
  // storage; approving it again retries the setup.
  let unfinished = $state<string | null>(null);
  // The administrator read the warning about answering an API another service serves.
  let sharingUnderstood = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const shown = $derived(
    updates.find((update) => update.updateId === focus) ??
      updates.find((update) => update.state === "pending"),
  );

  // The updates are reread whenever the page hands over its target again, as
  // the service page does each time it rereads the service. A decision's
  // outcome belongs to one target: only choosing another discards it.
  $effect(() => {
    if (scope.setKey(`${deploymentId}:${focus ?? ""}`)) {
      error = null;
      notice = null;
      unfinished = null;
      sharingUnderstood = false;
    }
    untrack(() => void load());
  });

  async function load(): Promise<void> {
    const token = scope.begin();
    try {
      const response = await traverseAll(async (page) => {
        const result = await trellis
          .serviceUpdatesList(
            apis.auth.API.actions["rpc:ServiceUpdates.List"].input.decode({
              deploymentId,
              page,
            }),
          )
          .orThrow();
        return { items: result.items, cursor: result.page.nextCursor };
      });
      if (!response.complete) throw response.error;
      let listed = [...response.items];
      if (focus && !listed.some((update) => update.updateId === focus)) {
        const { update } = await trellis
          .serviceUpdatesGet(
            apis.auth.API.actions["rpc:ServiceUpdates.Get"].input.decode({
              updateId: focus,
            }),
          )
          .orThrow();
        if (update.deploymentId === deploymentId) listed = [update, ...listed];
      }
      if (!scope.isCurrent(token)) return;
      updates = listed;
      loadError = null;
    } catch (cause) {
      if (scope.isCurrent(token))
        loadError = projectConsoleError(cause).message;
    } finally {
      scope.settle(token);
    }
  }

  async function loadTitles(): Promise<void> {
    if (!canPerform(authority.authority, "capabilitiesList")) return;
    const listed = await traverseAll(async (page) => {
      const response = await trellis.capabilitiesList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    titles = new Map(
      listed.items.map((item) => [String(item.capabilityId), item.title]),
    );
  }

  async function decide(update: Update, approve: boolean): Promise<void> {
    if (busy || !canPerform(authority.authority, "serviceUpdatesDecide"))
      return;
    const shared = update.changes.sharedApis.map(apiLabel);
    if (approve && shared.length) {
      if (!sharingUnderstood) return;
      const confirmed = await confirmationModal?.confirm({
        title: "Share these APIs with another service?",
        message: `Requests to ${shared.join(", ")} will go to either service at random. Each sees only some of them and keeps its own data.`,
        confirmLabel: "Approve and share",
      });
      if (!confirmed) return;
    }
    if (!approve) {
      const confirmed = await confirmationModal?.confirm({
        title: "Reject this update?",
        message:
          "The new build keeps waiting until it is replaced; the service keeps its current access.",
        confirmLabel: "Reject update",
      });
      if (!confirmed) return;
    }
    busy = true;
    error = null;
    notice = null;
    unfinished = null;
    try {
      const { update: decided } = await trellis
        .serviceUpdatesDecide({
          updateId: update.updateId,
          decision: approve ? "approved" : "declined",
          ...(approve && shared.length ? { acknowledgeSharedApis: true } : {}),
          expectedRevision: update.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      notice =
        decided.state === "applied"
          ? "Approved. The service connects with its new build now."
          : decided.state === "rejected"
            ? "Rejected. The service keeps its current build."
            : "The service changed since it asked. It will ask again when it reconnects.";
      await load();
      if (decided.state === "applied") onapplied?.();
    } catch (cause) {
      error =
        classifyMutationError(cause).kind === "unknown"
          ? "We couldn't confirm the decision. Refresh before trying again."
          : projectConsoleError(cause).message;
      if (approve) {
        await load();
        const current = updates.find(
          (item) => item.updateId === update.updateId,
        );
        if (current?.state === "applied") {
          unfinished = current.updateId;
          error =
            "Approved, but its storage isn't ready yet. The service waits until it is.";
        }
      }
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    void loadTitles();
    return () => scope.dispose();
  });
</script>

{#snippet changeList(title: string, items: string[], tone: string)}
  {#if items.length}
    <div>
      <h3 class="text-sm font-semibold">{title}</h3>
      <ul class="mt-1.5 space-y-1 text-sm">
        {#each items as item (item)}
          <li class="flex gap-2">
            <span class={["mt-1.5 size-1.5 shrink-0 rounded-full", tone]}
            ></span>{item}
          </li>
        {/each}
      </ul>
    </div>
  {/if}
{/snippet}

{#snippet stateBadge(update: Update)}
  <span
    class={[
      "badge badge-sm",
      update.state === "pending"
        ? "badge-warning"
        : update.state === "applied"
          ? "badge-success"
          : "badge-neutral",
    ]}
    >{update.state === "pending"
      ? "Waiting for approval"
      : update.state === "applied"
        ? "Installed"
        : update.state === "rejected"
          ? "Rejected"
          : "Replaced"}</span
  >
{/snippet}

{#if notice}<Notice variant="success">{notice}</Notice>{/if}
{#if loadError}<Notice variant="warning">{loadError}</Notice>{/if}
{#if error}<Notice variant="warning"
    >{error}{#if unfinished}{@const retry = updates.find(
        (item) => item.updateId === unfinished,
      )}{#if retry}<button
          class="btn btn-sm btn-outline ml-3"
          disabled={busy}
          onclick={() => void decide(retry, true)}>Try again</button
        >{/if}{/if}</Notice
  >{/if}
{#if shown}
  {@const changes = shown.changes}
  {@const described = describeUpdateChanges(changes, titles)}
  <section
    class={[
      "trellis-section space-y-4 p-5",
      shown.state === "pending" && "border-warning",
    ]}
    aria-label="Service update"
  >
    <div class="flex flex-wrap items-center justify-between gap-3">
      <div>
        <h2 class="text-base font-semibold">
          {shown.state === "pending"
            ? "A new build wants more access"
            : "Update from a new build"}
        </h2>
        <p class="text-sm text-base-content/65">
          Asked {formatDate(shown.requestedAt)}
        </p>
      </div>
      {@render stateBadge(shown)}
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      {@render changeList("Asks for", described.asks, "bg-warning")}
      {@render changeList("Gives up", described.givesUp, "bg-base-content/40")}
      {@render changeList("Provides", described.provides, "bg-info")}
    </div>
    {#if shown.state === "pending" && changes.sharedApis.length}
      <div class="alert alert-warning block space-y-2" role="alert">
        <p class="font-semibold">
          Another service already answers {changes.sharedApis
            .map(apiLabel)
            .join(", ")}.
        </p>
        <p class="text-sm">
          Approving makes both services answer the same requests. Each request
          goes to one of them at random, so each sees only some requests, they
          keep separate data, and callers can't tell which one answered. This is
          not how you add capacity or upgrade a service; add instances to one
          service for that.
        </p>
        <label class="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            class="checkbox checkbox-sm"
            bind:checked={sharingUnderstood}
          />
          I understand requests will be split between the two services
        </label>
      </div>
    {/if}
    {#if shown.state === "pending" && canPerform(authority.authority, "serviceUpdatesDecide")}
      <div class="trellis-action-row border-t border-base-300 pt-4">
        <button
          class="btn btn-outline btn-error"
          disabled={busy}
          onclick={() => void decide(shown, false)}>Reject</button
        >
        <button
          class="btn btn-primary"
          disabled={busy ||
            (changes.sharedApis.length > 0 && !sharingUnderstood)}
          onclick={() => void decide(shown, true)}>Approve update</button
        >
      </div>
    {/if}
  </section>
{/if}
<ConfirmationModal bind:this={confirmationModal} />
