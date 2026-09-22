<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { goto } from "$app/navigation";
  import { resolve } from "$lib/console_paths";
  import { onMount } from "svelte";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import ChoiceRow from "$lib/components/ChoiceRow.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import SelectionGroup from "$lib/components/SelectionGroup.svelte";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Group = apis.auth.CapabilityGroupsGetOutput["group"];
  type CapabilityListing = apis.auth.CapabilitiesListOutput["items"][number];

  let { mode, targetGroupKey = null }: {
    mode: "create" | "edit";
    targetGroupKey?: string | null;
  } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("capability-group");
  const putMutation = new MutationController<apis.auth.CapabilityGroupsPutInput, apis.auth.CapabilityGroupsPutOutput>();
  const deleteMutation = new MutationController<apis.auth.CapabilityGroupsDeleteInput, apis.auth.CapabilityGroupsDeleteOutput>();

  let loading = $state(true);
  let capabilitiesLoading = $state(false);
  let saving = $state(false);
  let uncertain = $state(false);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let saved = $state<string | null>(null);
  /** True when the requested group is genuinely absent. */
  let notFound = $state(false);
  let groups = $state.raw<Group[]>([]);
  let capabilityListings = $state.raw<CapabilityListing[]>([]);
  /** True when the discovery catalog could not be fully loaded. */
  let catalogIncomplete = $state(false);
  let selectedGroup = $state.raw<Group | null>(null);

  let formGroupKey = $state("");
  let formDisplayName = $state("");
  let formDescription = $state("");
  // The complete selected set, independent of catalog presentation. A partial
  // catalog must never turn an existing assignment into a deletion.
  let selectedCapabilities = $state<string[]>([]);
  let selectedIncludedGroups = $state<string[]>([]);
  let capabilityDraft = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();

  const editingExisting = $derived(mode === "edit");
  const isBuiltInSelection = $derived(selectedGroup?.groupKey === "admin");
  const busy = $derived(loading || saving || uncertain);
  const editable = $derived(!busy && !isBuiltInSelection);
  const catalogedCapabilityKeys = $derived(
    new Set(capabilityListings.map((capability) => capability.capability)),
  );
  const unresolvedCapabilities = $derived(
    selectedCapabilities.filter((key) => !catalogedCapabilityKeys.has(key)).sort(),
  );
  const sortedGroups = $derived(
    groups.slice().sort((left, right) => {
      if ((left.groupKey === "admin") !== (right.groupKey === "admin")) {
        return left.groupKey === "admin" ? -1 : 1;
      }
      return left.groupKey.localeCompare(right.groupKey);
    }),
  );

  function uniqueSorted(values: readonly string[]): string[] {
    return Array.from(new Set(values)).sort((left, right) => left.localeCompare(right));
  }

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  function localCapabilityKey(key: string): string {
    return key.includes("::") ? key.split("::").slice(1).join("::") : key;
  }

  function sourceApiLabel(sourceApi: string | null): string {
    return sourceApi ?? "trellis.auth@v1";
  }

  function applyGroup(group: Group): void {
    selectedGroup = group;
    formGroupKey = group.groupKey;
    formDisplayName = group.displayName;
    formDescription = group.description;
    selectedCapabilities = uniqueSorted(group.capabilities);
    selectedIncludedGroups = uniqueSorted(group.includedGroups);
  }

  function setCapabilitySelected(key: string, selected: boolean): void {
    selectedCapabilities = selected
      ? uniqueSorted([...selectedCapabilities, key])
      : selectedCapabilities.filter((value) => value !== key);
  }

  function addCapabilityDraft(): void {
    const candidate = capabilityDraft.trim();
    if (candidate === "") return;
    setCapabilitySelected(candidate, true);
    capabilityDraft = "";
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    if (!uncertain) error = null;
    notFound = false;
    saved = null;
    try {
      const groupsResult = await traverseAll<Group>(
        async (page) => {
          const response = await trellis.capabilityGroupsList({ page }).take();
          if (isErr(response)) throw response;
          return { items: response.items, cursor: response.page.nextCursor };
        },
      );
      if (!scope.isCurrent(token)) return;
      if (!groupsResult.complete) {
        error = failureFrom(groupsResult.error);
        return;
      }
      groups = [...groupsResult.items];

      if (!editingExisting) return;
      if (!targetGroupKey) {
        error = { message: "Group key is required." };
        notFound = true;
        return;
      }
      // Exact edit through Get, never a list scan.
      const response = await trellis.capabilityGroupsGet({ groupKey: targetGroupKey }).take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = failureFrom(response);
        notFound = error.code === "not_found" || error.code === "invalid_request";
        return;
      }
      applyGroup(response.group);
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }

  }

  async function loadCapabilities(): Promise<void> {
    capabilitiesLoading = true;
    catalogIncomplete = false;
    try {
      const result = await traverseAll<CapabilityListing>(
        async (page) => {
          const response = await trellis.capabilitiesList({ page: catalogPage(page.cursor) }).take();
          if (isErr(response)) throw response;
          return { items: response.items, cursor: response.page.nextCursor };
        },
      );
      capabilityListings = [...result.items];
      // A failed later page means the catalog is partial: keep the assignments
      // and say so, rather than presenting the partial set as complete.
      catalogIncomplete = !result.complete;
      if (!result.complete) {
        error = failureFrom(result.error);
      }
    } catch (cause) {
      capabilityListings = [];
      catalogIncomplete = true;
      error = failureFrom(cause);
    } finally {
      capabilitiesLoading = false;
    }
  }

  async function save(event: SubmitEvent | undefined): Promise<void> {
    event?.preventDefault();
    if (!editable) return;
    const groupKey = formGroupKey.trim();
    const displayName = formDisplayName.trim();
    const description = formDescription.trim();
    if (!groupKey || !displayName || !description) {
      error = { message: "Group key, display name, and description are required." };
      saved = null;
      return;
    }
    // Adding or removing included groups needs the complete dependency graph.
    if (
      !catalogIncomplete && selectedIncludedGroups.some((key) =>
        !groups.some((group) => group.groupKey === key)
      )
    ) {
      error = { message: "An included capability group is not in the loaded group list." };
      return;
    }

    const requestToken = scope.begin();
    const idempotencyKey = ulid();
    const intent = captureIntent<apis.auth.CapabilityGroupsPutInput>({
      operation: "capabilityGroupsPut",
      targetId: groupKey,
      label: displayName,
      idempotencyKey,
      input: {
        groupKey,
        displayName,
        description,
        // Uncataloged assignments remain selected until explicitly removed.
        capabilities: uniqueSorted(selectedCapabilities),
        includedGroups: uniqueSorted(selectedIncludedGroups.filter((key) => key !== groupKey)),
        expectedVersion: selectedGroup?.version ?? null,
        idempotencyKey,
      },
      scope: { routeKey: targetGroupKey ?? "group-new" },
    });
    if (!putMutation.begin(intent)) return;
    saving = true;
    error = null;
    saved = null;
    try {
      const outcome = await putMutation.send({
        isStillValid: () => scope.isCurrent(requestToken) && (targetGroupKey ?? "group-new") === intent.scope.routeKey &&

          (selectedGroup?.version ?? null) === intent.input.expectedVersion,
        dispatch: async ({ input }) => await trellis.capabilityGroupsPut(input).orThrow(),
      });
      if (!outcome) return;
      if (!scope.isCurrent(requestToken)) return;
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Group save outcome unknown. Inspect the group and reload before another change." };
        } else error = failureFrom(outcome.error);
        return;
      }
      const response = outcome.value;
      // The same projection used at load keeps the new version, so a second
      // save uses it instead of the stale pre-save version.
      applyGroup(response.group);
      saved = `Capability group ${groupKey} saved.`;
      if (!editingExisting) await goto(resolve("/admin/capability-groups"));
    } catch (cause) {
      error = failureFrom(cause);
    } finally {
      saving = false;
    }
  }

  async function remove(event: MouseEvent): Promise<void> {
    event.preventDefault();
    if (busy || selectedGroup === null) return;
    const requestToken = scope.begin();
    const idempotencyKey = ulid();
    const intent = captureIntent<apis.auth.CapabilityGroupsDeleteInput>({
      operation: "capabilityGroupsDelete",
      targetId: selectedGroup.groupKey,
      label: selectedGroup.displayName,
      idempotencyKey,
      input: {
        groupKey: selectedGroup.groupKey,
        expectedVersion: selectedGroup.version,
        idempotencyKey,
      },
      scope: { routeKey: targetGroupKey ?? "group-new" },
    });
    if (!deleteMutation.begin(intent)) return;
    saving = true;
    const confirmed = await confirmationModal?.confirm({
      title: "Delete capability group?",
      message: "This removes the capability group. Members of the group lose its assignments.",
      confirmLabel: "Delete group",
      targetLabel: "Group",
      targetName: intent.targetId,
      expectedValue: intent.targetId,
    });
    if (!confirmed) {
      deleteMutation.cancel();
      saving = false;
      return;
    }
    error = null;
    try {
      const outcome = await deleteMutation.send({
        isStillValid: () => scope.isCurrent(requestToken) && (targetGroupKey ?? "group-new") === intent.scope.routeKey &&

          selectedGroup?.groupKey === intent.targetId && selectedGroup.version === intent.input.expectedVersion,

        dispatch: async ({ input }) => await trellis.capabilityGroupsDelete(input).orThrow(),
      });
      if (!outcome) return;
      if (!scope.isCurrent(requestToken)) return;
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Group deletion outcome unknown. Inspect the group and reload before another change." };
        } else error = failureFrom(outcome.error);
        return;
      }
      await goto(resolve("/admin/capability-groups"));
    } catch (cause) {
      error = failureFrom(cause);
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    void load();
    void loadCapabilities();
    return () => scope.dispose();
  });
</script>

<section class="mx-auto max-w-5xl space-y-4">
  <div>
    <a class="btn btn-ghost btn-sm" href={resolve("/admin/capability-groups")}>Back to capability groups</a>
  </div>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
      <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={load}>Retry</button>
    </Notice>
  {/if}
  {#if saved}
    <Notice variant="success">{saved}</Notice>
  {/if}
  {#if catalogIncomplete && !capabilitiesLoading}
    <Notice variant="warning">
      The capability discovery catalog is incomplete. Existing assignments are preserved and
      shown under Unresolved assignments; operations that need the complete catalog are disabled.
    </Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading capability group" /></Panel>
  {:else if editingExisting && notFound}
    <EmptyState
      title="Capability group unavailable"
      description={`No capability group matches '${targetGroupKey ?? ""}'. It may have been removed or the link may be stale.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/capability-groups")}>Back to capability groups</a>
        <button class="btn btn-ghost btn-sm" type="button" onclick={load}>Retry</button>
      {/snippet}
    </EmptyState>
  {:else}
    <form class="space-y-4" onsubmit={save}>
      <Panel
        title={editingExisting ? formDisplayName : "New capability group"}
        eyebrow={isBuiltInSelection ? "Built-in" : "Group"}
      >
        {#snippet actions()}
          {#if isBuiltInSelection}
            <span class="badge badge-neutral badge-sm">read-only</span>
          {/if}
          <span class="trellis-metadata text-[0.65rem]">
            {selectedGroup ? `Version ${selectedGroup.version} · updated ${formatDate(selectedGroup.updatedAt)}` : "not saved"}
          </span>
        {/snippet}

        {#if isBuiltInSelection}
          <div class="mb-3 rounded border border-base-300 bg-base-100/50 px-3 py-2 text-xs text-base-content/65">
            The built-in admin group is managed by the platform and cannot be changed here.
          </div>
        {/if}

        <div class="grid gap-3 md:grid-cols-2">
          <label class="form-control">
            <span class="trellis-field-label">Group key</span>
            <input class="input input-bordered input-sm mt-1 w-full trellis-identifier" bind:value={formGroupKey} disabled={busy || editingExisting} required />
          </label>
          <label class="form-control">
            <span class="trellis-field-label">Display name</span>
            <input class="input input-bordered input-sm mt-1 w-full" bind:value={formDisplayName} disabled={!editable} required />
          </label>
          <label class="form-control md:col-span-2">
            <span class="trellis-field-label">Description</span>
            <textarea class="textarea textarea-bordered textarea-sm mt-1 min-h-20 w-full" bind:value={formDescription} disabled={!editable} required></textarea>
          </label>
        </div>
      </Panel>

      <Panel title="Capability assignments" eyebrow="Selected set">
        {#snippet actions()}
          {#if capabilitiesLoading}
            <span class="loading loading-spinner loading-xs text-base-content/45"></span>
          {:else}
            <span class="trellis-metadata text-[0.65rem]">{selectedCapabilities.length} selected</span>
          {/if}
        {/snippet}

        <div class="mb-3 flex flex-wrap gap-2">
          <input
            class="input input-bordered input-sm grow font-mono"
            bind:value={capabilityDraft}
            placeholder="exact::capability@v1::id"
            aria-label="Exact capability ID"
            disabled={!editable}
          />
          <button class="btn btn-outline btn-xs" type="button" disabled={!editable || capabilityDraft.trim() === ""} onclick={addCapabilityDraft}>
            Add exact ID
          </button>
        </div>

        {#if unresolvedCapabilities.length > 0}
          <div class="mb-3 rounded border border-warning/40 bg-warning/10 px-3 py-2">
            <div class="text-xs font-semibold uppercase tracking-wide">Unresolved assignments</div>
            <p class="mt-1 text-xs text-base-content/70">
              These IDs are assigned but not described by the discovery catalog. They stay
              selected and are submitted unchanged.
            </p>
            <ul class="mt-2 space-y-1">
              {#each unresolvedCapabilities as key (key)}
                <li class="flex items-center justify-between gap-2 text-xs">
                  <span class="trellis-identifier break-all">{key}</span>
                  <button
                    class="btn btn-ghost btn-xs text-error"
                    type="button"
                    disabled={!editable}
                    onclick={() => setCapabilitySelected(key, false)}
                  >Remove</button>
                </li>
              {/each}
            </ul>
          </div>
        {/if}

        <div class="max-h-[28rem] overflow-y-auto rounded border border-base-300 bg-base-100/40">
          {#each capabilityListings as capability (capability.capability)}
            <ChoiceRow>
              {#snippet input()}
                <input
                  class="checkbox checkbox-sm mt-0.5"
                  type="checkbox"
                  checked={selectedCapabilities.includes(capability.capability)}
                  disabled={!editable}
                  onchange={(event) => setCapabilitySelected(capability.capability, event.currentTarget.checked)}
                />
              {/snippet}
              <span class="min-w-0">
                <span class="block font-medium text-base-content">{capability.displayName}</span>
                <span class="trellis-identifier mt-0.5 block break-all text-base-content/50">{localCapabilityKey(capability.capability)}</span>
                <span class="mt-0.5 block text-base-content/60">{capability.description}</span>
                <span class="trellis-field-help block">Source API: {sourceApiLabel(capability.sourceApi)}</span>
              </span>
            </ChoiceRow>
          {:else}
            <div class="px-2 py-3 trellis-metadata text-xs">
              No cataloged capabilities were returned.
            </div>
          {/each}
        </div>
      </Panel>

      <Panel title="Included groups" eyebrow="Nested membership">
        <p class="trellis-field-help mb-2">
          Nested groups included by this group. Self-inclusion is rejected. Changing
          memberships requires the complete group list.
        </p>
        <SelectionGroup title="Included groups" count={selectedIncludedGroups.length} bodyClass="max-h-64 overflow-y-auto rounded border border-base-300 bg-base-100/40">
          {#each sortedGroups as group (group.groupKey)}
            {#if group.groupKey !== formGroupKey}
              <ChoiceRow>
                {#snippet input()}
                  <input
                    class="checkbox checkbox-sm mt-0.5"
                    type="checkbox"
                    checked={selectedIncludedGroups.includes(group.groupKey)}
                    disabled={!editable || catalogIncomplete}
                    onchange={(event) => {
                      const key = group.groupKey;
                      selectedIncludedGroups = event.currentTarget.checked
                        ? uniqueSorted([...selectedIncludedGroups, key])
                        : selectedIncludedGroups.filter((value) => value !== key);
                    }}
                  />
                {/snippet}
                <span class="min-w-0">
                  <span class="flex items-center gap-2">
                    <span class="trellis-identifier font-medium">{group.groupKey}</span>
                    {#if group.groupKey === "admin"}<span class="badge badge-neutral badge-xs">built-in</span>{/if}
                  </span>
                  <span class="mt-0.5 block truncate text-base-content/60" title={group.displayName}>{group.displayName}</span>
                  <span class="trellis-field-help block">{group.capabilities.length} capabilities, {group.includedGroups.length} included groups</span>
                </span>
              </ChoiceRow>
            {/if}
          {:else}
            <div class="px-2 py-3 trellis-metadata text-xs">No capability groups returned.</div>
          {/each}
        </SelectionGroup>
      </Panel>

      <div class="flex justify-end gap-2">
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/capability-groups")}>Cancel</a>
        {#if !isBuiltInSelection}
          {#if editingExisting}
            <button class="btn btn-error btn-outline btn-sm" type="button" disabled={busy} onclick={remove}>Delete group</button>
          {/if}
          <button class="btn btn-outline btn-sm" type="submit" disabled={busy || catalogIncomplete}>
            {saving ? "Saving" : "Save group"}
          </button>
        {/if}
      </div>
    </form>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
