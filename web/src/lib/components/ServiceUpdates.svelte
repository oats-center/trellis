<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { ulid } from "ulid";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { capabilityArea } from "$lib/console/capability_areas.ts";
  import { apiLabel, sentenceName } from "$lib/console/plain_labels.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import Notice from "$lib/components/Notice.svelte";

  type Update = apis.auth.ServiceUpdatesGetOutput["update"];
  type Resource = Update["changes"]["addedResources"][number];

  type Props = {
    deploymentId: string;
    /** The update a review link opened, shown even once it is decided. */
    focus?: string;
    /** Called after an approval installs the update. */
    onapplied?: () => void;
  };
  let { deploymentId, focus, onapplied }: Props = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("service-updates");
  const RESOURCE_KINDS: Record<string, string> = {
    kv: "Key-value store",
    store: "File store",
    state: "Saved state",
    job: "Job queue",
    eventConsumer: "Event delivery",
  };

  let updates = $state.raw<Update[]>([]);
  let titles = $state.raw<Map<string, string>>(new Map());
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
  const history = $derived(
    updates.filter((update) => update.updateId !== shown?.updateId),
  );

  $effect(() => {
    const target = `${deploymentId}:${focus ?? ""}`;
    scope.setKey(target);
    void load();
  });

  async function load(): Promise<void> {
    const token = scope.begin();
    try {
      const response = await trellis
        .serviceUpdatesList(
          apis.auth.API.actions["rpc:ServiceUpdates.List"].input.decode({
            deploymentId,
            page: { limit: 20 },
          }),
        )
        .orThrow();
      let listed = response.items;
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
      error = null;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
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

  function capabilityLabel(capabilityId: string): string {
    const title = titles.get(capabilityId);
    if (title) return title;
    const area = capabilityArea(capabilityId);
    const action = capabilityId.split(".").at(-1) ?? capabilityId;
    return `${area.title}: ${sentenceName(action).toLowerCase()} (${area.api})`;
  }

  function resourceLabel(resource: Resource): string {
    return `${RESOURCE_KINDS[resource.kind] ?? sentenceName(resource.kind)} “${resource.name}”`;
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
          idempotencyKey: ulid(),
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
      {@render changeList(
        "Asks for",
        [
          ...changes.addedCapabilities.map(capabilityLabel),
          ...changes.addedResources.map(resourceLabel),
          ...changes.reattachedResources.map(
            (resource) =>
              `${resourceLabel(resource)} again, with the data it kept`,
          ),
          ...changes.addedApis.map((api) =>
            changes.sharedApis.includes(api)
              ? `To offer ${apiLabel(api)}, which another service already offers`
              : `To offer ${apiLabel(api)}`,
          ),
          ...changes.changedResources.map(
            (resource) => `${resourceLabel(resource)}, with more room`,
          ),
        ],
        "bg-warning",
      )}
      {@render changeList(
        "Gives up",
        [
          ...changes.removedCapabilities.map(capabilityLabel),
          ...changes.removedResources.map(
            (resource) => `${resourceLabel(resource)} (its data is kept)`,
          ),
        ],
        "bg-base-content/40",
      )}
      {@render changeList(
        "Provides",
        [
          ...changes.removedApis.map(
            (api) => `Stops offering ${apiLabel(api)}`,
          ),
          ...changes.acceptedApis.map(
            (api) => `Installs the new version of ${apiLabel(api)}`,
          ),
        ],
        "bg-info",
      )}
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
{#if history.length}
  <details class="trellis-section px-5 py-3 text-sm">
    <summary class="cursor-pointer font-medium">Earlier updates</summary>
    <ul class="mt-2 divide-y divide-base-300">
      {#each history as update (update.updateId)}
        <li class="flex items-center justify-between gap-3 py-2">
          <span>{formatDate(update.requestedAt)}</span>
          {@render stateBadge(update)}
        </li>
      {/each}
    </ul>
  </details>
{/if}
<ConfirmationModal bind:this={confirmationModal} />
