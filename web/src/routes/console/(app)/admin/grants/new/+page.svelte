<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { goto } from "$app/navigation";
  import { resolve } from "$lib/console_paths";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { catalogPage, resolveExact, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, classifyMutationError, MutationController, isIntentCurrent } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ChoiceRow from "$lib/components/ChoiceRow.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import SelectionGroup from "$lib/components/SelectionGroup.svelte";
  import { hasDuplicateRoleMapping } from "$lib/portal-grants";
  import { getTrellis } from "$lib/trellis";

  type Capability = apis.auth.CapabilitiesListOutput["items"][number];
  type Group = apis.auth.CapabilityGroupsGetOutput["group"];
  type Portal = apis.auth.PortalsListOutput["items"][number];
  type Policy = apis.auth.PortalsGrantOverridesListOutput["items"][number];
  type RoleDraft = {
    id: string;
    providerId: string;
    role: string;
    directCapabilities: string;
    capabilityGroupKeys: string;
  };

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("portal-grant");
  const providerScope = new RequestScope("portal-grant-providers");
  type PutInput = Parameters<typeof trellis.portalsGrantOverridesPut>[0];
  type RemoveInput = Parameters<typeof trellis.portalsGrantOverridesRemove>[0];
  const removal = new MutationController<RemoveInput, unknown>((state) => saving = state.busy);

  let loading = $state(true);
  let saving = $state(false);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let readFailure = $state(false);
  let outcomeUnknown = $state(false);
  let portals = $state.raw<Portal[]>([]);
  let capabilities = $state.raw<Capability[]>([]);
  let groups = $state.raw<Group[]>([]);
  let catalogIncomplete = $state(false);
  let existing = $state.raw<Policy | null>(null);
  /** True when the requested tuple is genuinely absent. */
  let unavailable = $state(false);
  /** True when the URL pinned this edit to an exact tuple. */
  const pinnedTuple = $derived(
    page.url.searchParams.get("portalId") !== null || page.url.searchParams.get("participantId") !== null,

  );

  let portalId = $state("");
  let participantId = $state("");
  let directCapabilities = $state<string[]>([]);
  let capabilityGroupKeys = $state<string[]>([]);
  let roleMappings = $state<RoleDraft[]>([]);
  let providerIds = $state.raw<string[]>([]);
  let providerLoading = $state(false);
  let providerError = $state<string | null>(null);
  let confirmationModal: ConfirmationModal | undefined = $state();
  const requestedPortalId = $derived(page.url.searchParams.get("portalId"));
  const requestedParticipantId = $derived(page.url.searchParams.get("participantId"));

  const busy = $derived(loading || saving);
  const sortedCapabilities = $derived(
    capabilities.toSorted((left, right) => left.capability.localeCompare(right.capability)),
  );
  const sortedGroups = $derived(
    groups.toSorted((left, right) => left.groupKey.localeCompare(right.groupKey)),
  );
  const catalogedCapabilityKeys = $derived(
    new Set(capabilities.map((capability) => capability.capability)),
  );
  const catalogedGroupKeys = $derived(new Set(groups.map((group) => group.groupKey)));
  const unresolvedCapabilities = $derived(
    directCapabilities.filter((key) => !catalogedCapabilityKeys.has(key)).sort(),
  );
  const unresolvedGroups = $derived(
    capabilityGroupKeys.filter((key) => !catalogedGroupKeys.has(key)).sort(),
  );
  /** Role mappings whose provider is no longer in the configured provider list. */
  const missingProviderMappings = $derived(
    roleMappings.filter((mapping) =>
      mapping.providerId !== "" && !providerIds.includes(mapping.providerId)
    ),
  );
  const effectivePreview = $derived.by(() => {
    const selected = new Set(expand(directCapabilities, capabilityGroupKeys));
    for (const mapping of roleMappings) {
      for (
        const capability of expand(
          list(mapping.directCapabilities),
          list(mapping.capabilityGroupKeys),
        )
      ) {
        selected.add(capability);
      }
    }
    return [...selected].sort((left, right) => left.localeCompare(right));
  });

  function list(value: string): string[] {
    return [...new Set(value.split(/[\n,]/).map((item) => item.trim()).filter(Boolean))].sort();
  }

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  async function loadProviders(targetPortalId: string): Promise<void> {
    const token = providerScope.begin();
    providerLoading = true;
    providerError = null;
    providerIds = [];
    try {
      if (targetPortalId === "") {
        providerIds = [];
        return;
      }
      const response = await trellis.portalsGet({ portalId: targetPortalId }).take();
      if (!providerScope.isCurrent(token)) return;
      if (isErr(response)) {
        providerError = failureFrom(response).message;
        providerIds = [];
        return;
      }
      providerIds = response.portal.loginSettings.providers ?? [];
    } catch (cause) {
      if (!providerScope.isCurrent(token)) return;
      providerError = failureFrom(cause).message;
      providerIds = [];
    } finally {
      if (providerScope.settle(token)) providerLoading = false;
    }
  }

  function expand(direct: string[], groupKeys: string[]): string[] {
    const selected = new Set(direct);
    const groupMap = new Map(groups.map((group) => [group.groupKey, group]));
    const pending = [...groupKeys];
    const visited = new Set<string>();
    while (pending.length > 0) {
      const key = pending.pop();
      if (!key || visited.has(key)) continue;
      visited.add(key);
      const group = groupMap.get(key);
      if (!group) continue;
      for (const capability of group.capabilities) selected.add(capability);
      pending.push(...group.includedGroups);
    }
    return [...selected];
  }

  function addRoleMapping(): void {
    const provider = providerIds[0] ?? "";
    roleMappings = [
      ...roleMappings,
      {
        id: ulid(),
        providerId: provider,
        role: "",
        directCapabilities: "",
        capabilityGroupKeys: "",
      },
    ];
  }

  function applyPolicy(policy: Policy): void {
    existing = policy;
    portalId = policy.portalId;
    participantId = policy.participantId;
    directCapabilities = [...policy.directCapabilities];
    capabilityGroupKeys = [...policy.capabilityGroupKeys];
    roleMappings = policy.roleMappings.map((mapping) => ({
      id: ulid(),
      providerId: mapping.providerId,
      role: mapping.role,
      directCapabilities: mapping.directCapabilities.join(", "),
      capabilityGroupKeys: mapping.capabilityGroupKeys.join(", "),
    }));
  }

  async function load(targetPortalId: string | null, targetParticipantId: string | null): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    readFailure = false;
    outcomeUnknown = false;
    unavailable = false;
    catalogIncomplete = false;
    existing = null;
    portals = [];
    capabilities = [];
    groups = [];
    portalId = "";
    participantId = "";
    directCapabilities = [];
    capabilityGroupKeys = [];
    roleMappings = [];
    providerScope.invalidate();
    providerIds = [];
    if (
      ((targetPortalId !== null || targetParticipantId !== null) && false)) {
      error = { message: "Not permitted to read portal grant inputs." };
      readFailure = true;
      if (scope.settle(token)) loading = false;
      return;
    }
    try {
      const portalResult = await traverseAll<Portal>(async (pageRequest) => {
        const response = await trellis.portalsList({ page: pageRequest }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      if (!portalResult.complete) {
        error = failureFrom(portalResult.error);
        readFailure = true;
        return;
      }
      portals = [...portalResult.items];

      const capabilityResult = await traverseAll<Capability>(async (pageRequest) => {
        const response = await trellis.capabilitiesList({ page: pageRequest }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      capabilities = [...capabilityResult.items];
      catalogIncomplete = !capabilityResult.complete;

      const groupResult = await traverseAll<Group>(async (pageRequest) => {
        const response = await trellis.capabilityGroupsList({ page: pageRequest }).take();
        if (isErr(response)) throw response;
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      groups = [...groupResult.items];
      catalogIncomplete ||= !groupResult.complete;

      if (targetPortalId !== null || targetParticipantId !== null) {
        if (!targetPortalId || !targetParticipantId) {
          unavailable = true;
          return;
        }
        // An exact-tuple edit resolves its policy or reports it unavailable; it
        // never opens a default policy or a different tuple's create form.
        const resolved = await resolveExact<Policy>(
          async (pageRequest) => {
            const response = await trellis.portalsGrantOverridesList({
              ...(targetPortalId === null ? {} : { portalId: targetPortalId }),
              ...(targetParticipantId === null
                ? {}
                : { participantId: targetParticipantId }),
              page: catalogPage(pageRequest.cursor),
            }).take();
            if (isErr(response)) throw response;
            return { items: response.items, cursor: response.page.nextCursor };
          },
          (policy) =>
            policy.portalId === targetPortalId && policy.participantId === targetParticipantId,

        );
        if (!scope.isCurrent(token)) return;
        if (!resolved.complete) {
          error = failureFrom(resolved.error);
          readFailure = true;
          return;
        }
        if (resolved.item !== undefined) {
          applyPolicy(resolved.item);
        } else {
          unavailable = true;
          error = {
            message: `No portal grant policy exists for '${targetPortalId ?? ""}' / '${targetParticipantId ?? ""}'.`,
          };
        }
        if (resolved.item !== undefined) await loadProviders(targetPortalId);
      } else {
        // Creating a policy starts with an explicit empty selection.
        portalId = "";
        participantId = "";
        directCapabilities = [];
        capabilityGroupKeys = [];
        roleMappings = [];
        providerIds = [];
      }
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
      readFailure = true;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (busy || unavailable || readFailure || outcomeUnknown) return;
    if (scope.key !== JSON.stringify([requestedPortalId, requestedParticipantId])) return;
    error = null;
    if (portalId === "" || participantId.trim() === "") {
      error = { message: "Portal and participant are required." };
      return;
    }
    const mappings = roleMappings.map((mapping) => ({
      providerId: mapping.providerId.trim(),
      role: mapping.role.trim(),
      directCapabilities: list(mapping.directCapabilities),
      capabilityGroupKeys: list(mapping.capabilityGroupKeys),
    }));
    if (mappings.some((mapping) => !mapping.providerId || !mapping.role)) {
      error = { message: "Every role mapping needs a provider and an exact role." };
      return;
    }
    if (hasDuplicateRoleMapping(mappings)) {
      error = { message: "Each provider and role pair may appear only once." };
      return;
    }
    if (existing !== null && existing.portalId !== portalId) {
      return;
    }

    const key = ulid();
    const intent = captureIntent<PutInput>({
      operation: "portalsGrantOverridesPut", targetId: `${portalId} / ${participantId.trim()}`,
      label: `${portalId} / ${participantId.trim()}`, idempotencyKey: key,
      input: {
        portalId, participantId: participantId.trim(),
        directCapabilities: [...directCapabilities].sort(),
        capabilityGroupKeys: [...capabilityGroupKeys].sort(), roleMappings: mappings,
        expectedVersion: existing?.version ?? null, idempotencyKey: key,
      },
      scope: { routeKey: scope.key },
    });
    saving = true;
    try {
      if (!isIntentCurrent(intent, { routeKey: scope.key }) || scope.key !== JSON.stringify([requestedPortalId, requestedParticipantId]) ||

        false) return;
      const response = await trellis.portalsGrantOverridesPut(intent.input).take();
      if (scope.key !== intent.scope.routeKey) return;
      if (isErr(response)) {
        outcomeUnknown = classifyMutationError(response.error).kind === "unknown";
        error = outcomeUnknown
          ? { message: "Outcome unknown; the policy may have been saved. Check the grants list before another operation." }
          : failureFrom(response);
        return;
      }
      applyPolicy(response.policy);
      await goto(resolve("/admin/grants"));
    } catch (cause) {
      if (scope.key === intent.scope.routeKey) {
        const outcome = classifyMutationError(cause);
        outcomeUnknown = outcome.kind === "unknown";
        error = outcomeUnknown
          ? { message: "Outcome unknown; the policy may have been saved. Check the grants list before another operation." }
          : failureFrom(cause);
      }
    } finally {
      saving = false;
    }
  }

  async function remove(): Promise<void> {
    if (existing === null || busy || unavailable || readFailure || outcomeUnknown) return;
    const key = ulid();
    const intent = captureIntent<RemoveInput>({
      operation: "portalsGrantOverridesRemove", targetId: `${existing.portalId} / ${existing.participantId}`,
      label: `${existing.portalId} / ${existing.participantId}`, expectedValue: existing.participantId,
      idempotencyKey: key,
      input: { portalId: existing.portalId, participantId: existing.participantId, expectedVersion: existing.version, idempotencyKey: key },
      scope: { routeKey: scope.key },
    });
    if (!removal.begin(intent)) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Remove portal grant policy?",
      message: "This removes the configured policy. Ordinary portal consent applies again.",
      confirmLabel: "Remove policy",
      targetLabel: "Policy",
      targetName: intent.label,
      expectedValue: intent.expectedValue,
    });
    if (!confirmed) { removal.cancel(); return; }
    error = null;
    const outcome = await removal.send({
      isStillValid: () => isIntentCurrent(intent, { routeKey: scope.key }) && requestedPortalId === intent.input.portalId && requestedParticipantId === intent.input.participantId &&

        existing?.version === intent.input.expectedVersion && !loading && !unavailable,
      dispatch: async ({ input }) => await trellis.portalsGrantOverridesRemove(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") {
      await goto(resolve("/admin/grants"));
    } else if (outcome.kind === "unknown") {
      outcomeUnknown = true;
      error = { message: "Outcome unknown; the removal may have completed. Check the grants list before another operation." };
    } else {
      error = failureFrom(outcome.error);
    }
  }

  $effect(() => {
    const portal = requestedPortalId;
    const participant = requestedParticipantId;
    scope.setKey(JSON.stringify([portal, participant]));
    untrack(() => confirmationModal?.cancel());
    removal.cancel();
    untrack(() => void load(portal, participant));
    return () => { scope.invalidate(); providerScope.invalidate(); };
  });
  onDestroy(() => { scope.dispose(); providerScope.dispose(); });
</script>

<section class="mx-auto max-w-6xl space-y-4">
  <a class="btn btn-ghost btn-sm" href={resolve("/admin/grants")}>Back to portal grants</a>
  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
      {#if readFailure}
        <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={() => void load(requestedPortalId, requestedParticipantId)}>Retry</button>
      {/if}
    </Notice>
  {/if}

  {#if loading && !error}
    <Panel><LoadingState label="Loading portal policy inputs" /></Panel>
  {:else if readFailure}
    <Panel><p class="text-sm text-base-content/60">Portal grant inputs are incomplete. Retry before editing.</p></Panel>
  {:else if unavailable}
    <EmptyState
      title="Portal grant policy unavailable"
      description="The requested portal and participant tuple has no policy. Create a policy from the grants list, or open an existing one."
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/grants")}>Back to portal grants</a>
      {/snippet}
    </EmptyState>
  {:else}
    {#if catalogIncomplete}
      <Notice variant="warning">
        The capability or group discovery catalog is incomplete. Unresolved references are
        preserved and shown below; they are never dropped on save.
      </Notice>
    {/if}
    {#if providerError}
      <Notice variant="warning">Portal providers could not be loaded: {providerError}</Notice>
    {/if}

    <form class="space-y-4" onsubmit={save}>
      <Panel
        title={existing ? "Edit portal grant policy" : "New portal grant policy"}
        eyebrow="Trusted browser authority"
      >
        {#snippet actions()}
          {#if existing}
            <span class="trellis-metadata text-[0.65rem]">Version {existing.version}</span>
          {/if}
        {/snippet}
        <div class="grid gap-3 md:grid-cols-2">
          <label class="form-control">
            <span class="trellis-field-label">Login portal</span>
            <select
              class="select select-bordered select-sm mt-1"
              value={portalId}
              onchange={(event) => {
                portalId = event.currentTarget.value;
                 void loadProviders(portalId);
              }}
              disabled={busy || existing !== null}
              required
            >
              <option value="">Select portal...</option>
              {#each portals as portal (portal.portalId)}
                <option value={portal.portalId} disabled={portal.disabled}>
                  {portal.displayName} · {portal.portalId}
                </option>
              {/each}
            </select>
          </label>
          <label class="form-control">
            <span class="trellis-field-label">Application participant</span>
            <input
              class="input input-bordered input-sm trellis-identifier mt-1"
              bind:value={participantId}
              disabled={busy || existing !== null}
              placeholder="example.app@v1"
              required
            />
          </label>
        </div>
        <p class="trellis-field-help mt-2">
          The policy is exact to this portal and participant. Missing policies always fall
          back to ordinary consent.
        </p>
      </Panel>

      <div class="grid gap-3 lg:grid-cols-2">
        <Panel title="Base capability groups" eyebrow="All logins">
          <SelectionGroup title="Capability groups" count={capabilityGroupKeys.length} bodyClass="max-h-72 overflow-y-auto rounded border border-base-300 bg-base-100/40">
            {#each sortedGroups as group (group.groupKey)}
              <ChoiceRow>
                {#snippet input()}
                  <input
                    class="checkbox checkbox-sm"
                    type="checkbox"
                    checked={capabilityGroupKeys.includes(group.groupKey)}
                    disabled={busy}
                    onchange={(event) => {
                      const key = group.groupKey;
                      capabilityGroupKeys = event.currentTarget.checked
                        ? [...new Set([...capabilityGroupKeys, key])].sort()
                        : capabilityGroupKeys.filter((value) => value !== key);
                    }}
                  />
                {/snippet}
                <span class="min-w-0">
                  <span class="trellis-identifier block truncate">{group.groupKey}</span>
                  <span class="trellis-field-help">{group.capabilities.length} direct · {group.includedGroups.length} nested</span>
                </span>
              </ChoiceRow>
            {:else}
              <p class="p-3 text-xs text-base-content/55">No capability groups.</p>
            {/each}
            {#each unresolvedGroups as key (key)}
              <ChoiceRow class="border-t">
                {#snippet input()}
                  <input
                    class="checkbox checkbox-sm"
                    type="checkbox"
                    checked
                    disabled={busy}
                    onchange={() => {
                      capabilityGroupKeys = capabilityGroupKeys.filter((value) => value !== key);
                    }}
                  />
                {/snippet}
                <span class="min-w-0">
                  <span class="block font-medium">Unresolved group reference</span>
                  <span class="trellis-identifier mt-0.5 block break-all text-base-content/50">{key}</span>
                </span>
              </ChoiceRow>
            {/each}
          </SelectionGroup>
        </Panel>
        <Panel title="Base direct capabilities" eyebrow="All logins">
          <SelectionGroup title="Direct capabilities" count={directCapabilities.length} bodyClass="max-h-72 overflow-y-auto rounded border border-base-300 bg-base-100/40">
            {#each sortedCapabilities as capability (capability.capability)}
              <ChoiceRow>
                {#snippet input()}
                  <input
                    class="checkbox checkbox-sm"
                    type="checkbox"
                    checked={directCapabilities.includes(capability.capability)}
                    disabled={busy}
                    onchange={(event) => {
                      const key = capability.capability;
                      directCapabilities = event.currentTarget.checked
                        ? [...new Set([...directCapabilities, key])].sort()
                        : directCapabilities.filter((value) => value !== key);
                    }}
                  />
                {/snippet}
                <span class="min-w-0">
                  <span class="trellis-identifier block truncate" title={capability.capability}>{capability.capability}</span>
                  <span class="trellis-field-help">{capability.description}</span>
                </span>
              </ChoiceRow>
            {:else}
              <p class="p-3 text-xs text-base-content/55">No capabilities.</p>
            {/each}
            {#each unresolvedCapabilities as key (key)}
              <ChoiceRow class="border-t">
                {#snippet input()}
                  <input
                    class="checkbox checkbox-sm"
                    type="checkbox"
                    checked
                    disabled={busy}
                    onchange={() => {
                      directCapabilities = directCapabilities.filter((value) => value !== key);
                    }}
                  />
                {/snippet}
                <span class="min-w-0">
                  <span class="block font-medium">Unresolved capability reference</span>
                  <span class="trellis-identifier mt-0.5 block break-all text-base-content/50">{key}</span>
                </span>
              </ChoiceRow>
            {/each}
          </SelectionGroup>
        </Panel>
      </div>

      <Panel title="Provider role mappings" eyebrow="Exact verified roles">
        {#snippet actions()}
          <span class="trellis-metadata text-[0.65rem]">{providerIds.length} configured provider{providerIds.length === 1 ? "" : "s"}</span>
          <button class="btn btn-outline btn-xs" type="button" onclick={addRoleMapping} disabled={busy}>Add role</button>
        {/snippet}
        {#if providerLoading}
          <p class="mb-2 text-xs text-base-content/55">Loading configured providers…</p>
        {/if}
        {#if missingProviderMappings.length > 0}
          <Notice variant="warning" class="mb-2">
            A role mapping references a provider that is not in the portal's configured
            provider list. It is preserved and submitted unchanged; remove it explicitly if
            it should not remain.
          </Notice>
        {/if}
        <DataTable size="xs" fixed class="min-w-[900px] border border-base-300">
          <colgroup><col style="width: 18%" /><col style="width: 18%" /><col style="width: 29%" /><col style="width: 29%" /><col style="width: 6%" /></colgroup>
          <thead><tr><th>Provider</th><th>Exact role</th><th>Direct capabilities</th><th>Capability groups</th><th></th></tr></thead>
          <tbody>
            {#each roleMappings as mapping (mapping.id)}
              <tr>
                <td>
                  <input
                    class="input input-bordered input-xs trellis-identifier w-full"
                    bind:value={mapping.providerId}
                    placeholder="provider-id"
                    disabled={busy}
                    aria-label="Provider ID"
                  />
                </td>
                <td><input class="input input-bordered input-xs trellis-identifier w-full" bind:value={mapping.role} placeholder="Engineering" disabled={busy} required /></td>
                <td><textarea class="textarea textarea-bordered textarea-xs trellis-identifier min-h-14 w-full" bind:value={mapping.directCapabilities} placeholder="api::read, api::write" disabled={busy}></textarea></td>
                <td><textarea class="textarea textarea-bordered textarea-xs trellis-identifier min-h-14 w-full" bind:value={mapping.capabilityGroupKeys} placeholder="operators, auditors" disabled={busy}></textarea></td>
                <td class="text-right"><button class="btn btn-ghost btn-xs" type="button" onclick={() => roleMappings = roleMappings.filter((item) => item.id !== mapping.id)} disabled={busy}>Remove</button></td>
              </tr>
            {:else}
              <tr><td colspan="5" class="text-base-content/55">No role mappings. Base policy applies to every authenticated provider identity.</td></tr>
            {/each}
          </tbody>
        </DataTable>
      </Panel>

      <Panel title="Effective preview" eyebrow="Configured upper bound">
        <div class="flex items-start justify-between gap-4">
          <div>
            <p class="text-sm font-medium">{effectivePreview.length} configured capabilities</p>
            <p class="trellis-field-help">
              Configured upper bound only. Provider evidence, the selected role, participant
              requirements, and server authorization determine actual grants.
            </p>
            <div class="trellis-identifier mt-2 max-h-28 overflow-auto text-xs text-base-content/65">
              {effectivePreview.join(", ") || "Required participant authority only"}
            </div>
          </div>
          <div class="flex shrink-0 gap-2">
            <a class="btn btn-ghost btn-sm" href={resolve("/admin/grants")}>Cancel</a>
            {#if existing}
               <button class="btn btn-error btn-outline btn-sm" type="button" disabled={busy || outcomeUnknown} onclick={remove}>Remove policy</button>
            {/if}
             <button class="btn btn-primary btn-sm" type="submit" disabled={busy || outcomeUnknown}>
              {saving ? "Saving..." : "Save policy"}
            </button>
          </div>
        </div>
      </Panel>
    </form>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
