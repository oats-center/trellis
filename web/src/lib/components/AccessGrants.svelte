<script lang="ts">
  import type { apis } from "trellis-web-generated";
  import { onDestroy, untrack } from "svelte";
  import { beforeNavigate } from "$app/navigation";
  import { SvelteMap, SvelteSet } from "svelte/reactivity";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { permissionKey, permissionLabel, presetPermissions, type Permission, type Capability, type CapabilityGroup } from "$lib/console/access.ts";
  import { errorMessage, formatDate } from "$lib/format";
  import Panel from "./Panel.svelte";
  import Notice from "./Notice.svelte";
  import LoadingState from "./LoadingState.svelte";
  import CopyButton from "./CopyButton.svelte";

  type Binding = apis.auth.GrantsGetOutput["binding"] & {};
  type Participant = apis.auth.ParticipantsListOutput["items"][number];
  type Detail = apis.auth.ParticipantsGetOutput["participant"];
  type SetInput = apis.auth.GrantsSetInput;
  type RevokeInput = apis.auth.GrantsRevokeInput;

  let { ownerKind = "user", ownerId, ownerLabel, disabled = false, bootstrapAdministrator = false, participantId: fixedParticipantId }: {
    ownerKind?: "user" | "deployment";
    ownerId: string;
    ownerLabel: string;
    disabled?: boolean;
    bootstrapAdministrator?: boolean;
    participantId?: string;
  } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("access-grants");
  const setMutation = new MutationController<SetInput, apis.auth.GrantsSetOutput>((state) => pending = state.busy);
  const revokeMutation = new MutationController<RevokeInput, apis.auth.GrantsRevokeOutput>((state) => pending = state.busy);
  const selected = new SvelteSet<string>();
  let pending = $state(false);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);
  let uncertain = $state(false);
  let bindings = $state.raw<Binding[]>([]);
  let participants = $state.raw<Participant[]>([]);
  let capabilities = $state.raw<Capability[]>([]);
  let groups = $state.raw<CapabilityGroup[]>([]);
  let catalogError = $state<string | null>(null);
  let editing = $state(false);
  let original = $state.raw<Binding | null>(null);
  let participantId = $state("");
  let detail = $state.raw<Detail | null>(null);
  let detailLoading = $state(false);
  let editorError = $state<string | null>(null);
  let expiry = $state("");
  let platformAdmin = $state(false);
  let review = $state(false);
  let revokeTarget = $state.raw<Binding | null>(null);
  let editorGeneration = 0;
  beforeNavigate((navigation) => {
    if (editing && !pending && !window.confirm("Leave without saving the access-grant draft?")) navigation.cancel();
  });

  const available = $derived.by(() => {
    const permissions = new SvelteMap<string, Permission>();
    for (const permission of [...detail?.requiredGrants.permissions ?? [], ...detail?.optionalBundles.flatMap((bundle) => bundle.permissions) ?? [], ...original?.grants.permissions ?? []]) {
      permissions.set(permissionKey(permission), permission);
    }
    return [...permissions.values()];
  });
  const selectedPermissions = $derived(available.filter((permission) => selected.has(permissionKey(permission))));
  const previousKeys = $derived(new Set(original?.grants.permissions.map(permissionKey) ?? []));
  const added = $derived(selectedPermissions.filter((permission) => !previousKeys.has(permissionKey(permission))));
  const removed = $derived(original?.grants.permissions.filter((permission) => !selected.has(permissionKey(permission))) ?? []);
  const busy = $derived(disabled || loading || pending || uncertain);
  const permissionCapabilities = $derived(capabilities.filter((capability) => capability.allows.length > 0 && capability.allows.every((permission) => available.some((candidate) => permissionKey(candidate) === permissionKey(permission)))));

  function protectedBinding(binding: Binding): boolean {
    return bootstrapAdministrator && binding.platformPrivileges.includes("trellis.auth::admin");
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    const target = { ownerKind, ownerId, participantId: fixedParticipantId };
    loading = true;
    error = null;
    try {
      const result = await traverseAll<Binding>(async (request) => {
        const response = await trellis.grantsList({ ...target, page: catalogPage(request.cursor) }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      if (!result.complete) throw result.error;
      bindings = [...result.items];
      uncertain = false;
    } catch (cause) {
      if (scope.isCurrent(token)) error = errorMessage(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function loadCatalog(): Promise<void> {
    const key = scope.key;
    try {
      const [participantResult, capabilityResult, groupResult] = await Promise.all([
        traverseAll<Participant>(async (request) => {
          const response = await trellis.participantsList({ page: catalogPage(request.cursor) }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        traverseAll<Capability>(async (request) => {
          const response = await trellis.capabilitiesList({ page: catalogPage(request.cursor) }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        traverseAll<CapabilityGroup>(async (request) => {
          const response = await trellis.capabilityGroupsList({ page: catalogPage(request.cursor) }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
      ]);
      if (scope.key !== key) return;
      if (!participantResult.complete) throw participantResult.error;
      if (!capabilityResult.complete) throw capabilityResult.error;
      if (!groupResult.complete) throw groupResult.error;
      participants = [...participantResult.items].filter((entry) => fixedParticipantId ? entry.participantId === fixedParticipantId : ownerKind === "user" ? entry.participantKind === "app" || entry.participantKind === "agent" : true);
      capabilities = [...capabilityResult.items];
      groups = [...groupResult.items];
      catalogError = null;
    } catch (cause) {
      if (scope.key === key) catalogError = errorMessage(cause);
    }
  }

  async function openEditor(binding: Binding | null, id = fixedParticipantId ?? ""): Promise<void> {
    if (busy || (binding && protectedBinding(binding))) return;
    const generation = ++editorGeneration;
    const key = scope.key;
    original = binding;
    participantId = binding?.participantId ?? id;
    selected.clear();
    for (const permission of binding?.grants.permissions ?? []) selected.add(permissionKey(permission));
    expiry = binding?.expiresAt == null ? "" : new Date(Number(binding.expiresAt)).toLocaleString("sv-SE").slice(0, 16).replace(" ", "T");
    platformAdmin = binding?.platformPrivileges.includes("trellis.auth::admin") ?? false;
    detail = null;
    editing = true;
    review = false;
    editorError = null;
    saved = null;
    revokeTarget = null;
    if (participantId === "") return;
    detailLoading = true;
    try {
      const response = await trellis.participantsGet({ participantId, ...(binding ? { revision: binding.installedRevision } : {}) }).orThrow();
      if (scope.key !== key || editorGeneration !== generation) return;
      detail = response.participant;
      if (!binding) for (const permission of detail.requiredGrants.permissions) selected.add(permissionKey(permission));
    } catch (cause) {
      if (scope.key === key && editorGeneration === generation) editorError = errorMessage(cause);
    } finally {
      if (scope.key === key && editorGeneration === generation) detailLoading = false;
    }
  }

  function applyPreset(key: string): void {
    editorError = null;
    review = false;
    try {
      const permissions = presetPermissions(key, groups, capabilities);
      const unavailable = permissions.filter((permission) => !available.some((candidate) => permissionKey(candidate) === permissionKey(permission)));
      if (unavailable.length) throw new Error(`Preset is incompatible with this installed participant: ${unavailable.map(permissionLabel).join("; ")}`);
      for (const permission of permissions) selected.add(permissionKey(permission));
    } catch (cause) { editorError = errorMessage(cause); }
  }

  async function saveAccess(): Promise<void> {
    if (busy || !detail || detailLoading || editorError || !review || catalogError || !canPerform(authority.authority, "grantsSet")) return;
    const key = ulid();
    const expiryMillis = expiry === "" ? null : new Date(expiry).getTime();
    if (expiryMillis !== null && !Number.isFinite(expiryMillis)) { editorError = "Enter a valid expiry date."; review = false; return; }
    const expiresAt = expiryMillis === null ? null : BigInt(expiryMillis);
    if (expiresAt !== null && expiresAt <= BigInt(Date.now())) { editorError = "Choose an expiry in the future, or leave it empty for no expiry."; review = false; return; }
    const input: SetInput = {
      ownerKind, ownerId, participantId: detail.participantId, installedRevision: detail.revision,
      grants: { format: detail.requiredGrants.format, permissions: selectedPermissions },
      platformPrivileges: [...original?.platformPrivileges.filter((privilege) => privilege !== "trellis.auth::admin") ?? [], ...platformAdmin ? ["trellis.auth::admin"] : []],
      expiresAt, expectedRevision: original?.revision ?? 0n, idempotencyKey: key,
    };
    const intent = captureIntent<SetInput>({ operation: "grantsSet", input, targetId: ownerId, label: ownerLabel, idempotencyKey: key, scope: { routeKey: scope.key } });
    if (!setMutation.begin(intent)) return;
    const generation = editorGeneration;
    const outcome = await setMutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && !disabled && editing && generation === editorGeneration,
      dispatch: ({ input }) => trellis.grantsSet(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") {
      bindings = [...bindings.filter((binding) => binding.participantId !== outcome.value.binding.participantId), outcome.value.binding];
      editing = false;
      saved = "Access saved. The permissions below are the server-confirmed grant.";
    } else if (outcome.kind === "unknown") { uncertain = true; error = "Result unknown. Refresh access to verify the grant before submitting another change."; }
    else { editorError = errorMessage(outcome.error); review = false; }
  }

  async function revoke(): Promise<void> {
    const binding = revokeTarget;
    if (!binding || busy || protectedBinding(binding) || !canPerform(authority.authority, "grantsRevoke")) return;
    const key = ulid();
    const input: RevokeInput = { ownerKind, ownerId, participantId: binding.participantId, expectedRevision: binding.revision, idempotencyKey: key, reason: "Revoked from Console" };
    const intent = captureIntent<RevokeInput>({ operation: "grantsRevoke", input, targetId: ownerId, label: ownerLabel, idempotencyKey: key, scope: { routeKey: scope.key } });
    if (!revokeMutation.begin(intent)) return;
    const outcome = await revokeMutation.send({
      isStillValid: () => scope.key === intent.scope.routeKey && !disabled && revokeTarget?.revision === input.expectedRevision,
      dispatch: ({ input }) => trellis.grantsRevoke(input).orThrow(),
    });
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") {
      bindings = bindings.map((entry) => entry.participantId === binding.participantId ? outcome.value.binding : entry);
      revokeTarget = null;
      saved = "Access revoked.";
    } else if (outcome.kind === "unknown") { uncertain = true; error = "Result unknown. Refresh access before retrying."; }
    else error = errorMessage(outcome.error);
  }

  $effect(() => {
    const key = JSON.stringify([ownerKind, ownerId, fixedParticipantId]);
    scope.setKey(key);
    setMutation.cancel(); revokeMutation.cancel();
    untrack(() => {
      bindings = []; editing = false; revokeTarget = null; detail = null; saved = null; catalogError = "Loading permission catalog.";
      ++editorGeneration;
      void load(); void loadCatalog();
    });
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<Panel title="Access grants">
  {#snippet actions()}
    <button class="btn btn-ghost btn-sm" type="button" disabled={pending || loading} onclick={() => { void load(); void loadCatalog(); }}>Refresh access</button>
    <button class="btn btn-outline btn-sm" type="button" disabled={busy || !!catalogError || !canPerform(authority.authority, "grantsSet")} onclick={() => void openEditor(null)}>Add grant</button>
  {/snippet}
  <p class="trellis-field-help mb-3">{ownerKind === "user" ? "Access belongs to this user and a specific application, not to the user globally." : "Deployment-owned access applies to its service or device instances."}</p>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if saved}<Notice variant="success">{saved}</Notice>{/if}
  {#if catalogError}<p class="trellis-field-help px-4 py-2">Permission editor unavailable: {catalogError}</p>{/if}
  {#if loading}<LoadingState label="Loading access grants" />
  {:else if !error || bindings.length}
    <div class="overflow-x-auto">
      <table class="table table-sm w-full">
        <thead><tr><th>Application / participant</th><th>Permissions</th><th>State / source</th><th>Expiry</th><th><span class="sr-only">Actions</span></th></tr></thead>
        <tbody>
          {#each bindings as binding (binding.participantId)}
            <tr>
              <td class="align-top"><span class="trellis-identifier">{binding.participantId}</span><CopyButton value={binding.participantId} label="Copy participant ID" />
                <p class="trellis-metadata">Installed revision {String(binding.installedRevision)} · Grant revision {String(binding.revision)}</p>
              </td>
              <td class="align-top">
                {#each binding.grants.permissions as permission (permissionKey(permission))}<p class="text-xs break-words">{permissionLabel(permission)}</p>{:else}<span class="text-xs opacity-60">No application permissions</span>{/each}
                {#if binding.platformPrivileges.length}<p class="mt-1 text-xs font-semibold">Platform: {binding.platformPrivileges.join(", ")}</p>{/if}
              </td>
              <td class="align-top"><span class="badge badge-sm {binding.state === 'active' ? 'badge-success' : 'badge-neutral'}">{binding.state}</span>
                <p class="trellis-metadata mt-1">{protectedBinding(binding) ? "Protected bootstrap administrator" : binding.provenance ? `Login policy: ${binding.provenance.portalId}` : binding.approvalMode === "exact" ? "Exact permission snapshot" : "Capability approval"}</p>
                <p class="trellis-metadata">Updated {formatDate(binding.updatedAt)}</p>
              </td>
              <td class="align-top text-xs">{binding.expiresAt === null ? "Never" : formatDate(binding.expiresAt)}</td>
              <td class="align-top"><div class="flex gap-1">
                <button class="btn btn-ghost btn-sm" type="button" disabled={busy || protectedBinding(binding) || !!catalogError || !canPerform(authority.authority, "grantsSet")} onclick={() => void openEditor(binding)}>Edit access</button>
                <button class="btn btn-ghost btn-sm text-error" type="button" disabled={busy || protectedBinding(binding) || binding.state !== "active" || !canPerform(authority.authority, "grantsRevoke")} onclick={() => { revokeTarget = binding; editing = false; }}>Revoke</button>
              </div></td>
            </tr>
          {:else}<tr><td colspan="5" class="py-5 text-sm text-base-content/60">No access grants for this {ownerKind}. Add a grant to permit access to a specific participant.</td></tr>{/each}
        </tbody>
      </table>
    </div>
  {/if}
  {#if revokeTarget}
    <section class="border-t border-base-300 p-4 space-y-3">
      <p class="text-sm font-semibold">Revoke {ownerLabel}'s access to <code>{revokeTarget.participantId}</code>?</p>
      <p class="trellis-field-help">Existing authorization for this grant will be revoked. Other applications are unaffected.</p>
      <div class="flex gap-2"><button class="btn btn-error btn-outline btn-sm" disabled={busy} onclick={() => void revoke()}>Confirm revoke</button><button class="btn btn-ghost btn-sm" disabled={pending} onclick={() => revokeTarget = null}>Cancel</button></div>
    </section>
  {/if}
  {#if editing}
    <section class="border-t border-base-300 p-4 space-y-4">
      <div class="flex items-center justify-between gap-3"><h3 class="font-semibold">{original ? "Edit access" : "Add access"}</h3><button type="button" class="btn btn-ghost btn-sm" disabled={pending} onclick={() => { editing = false; ++editorGeneration; }}>Cancel</button></div>
      <label class="form-control max-w-2xl"><span class="trellis-field-label">Application / participant</span>
        <select class="select select-bordered select-sm mt-1" value={participantId} disabled={pending || !!original || !!fixedParticipantId} onchange={(event) => void openEditor(bindings.find((entry) => entry.participantId === event.currentTarget.value) ?? null, event.currentTarget.value)}>
          <option value="">Choose an installed participant</option>
          {#if original && !participants.some((entry) => entry.participantId === original?.participantId)}<option value={original.participantId}>{original.participantId}</option>{/if}
          {#each participants as participant (participant.participantId)}<option value={participant.participantId}>{participant.participantId} · revision {String(participant.revision)}</option>{/each}
        </select>
      </label>
      {#if editorError}<Notice variant="error">{editorError}</Notice>{/if}
      {#if detailLoading}<LoadingState label="Loading installed permissions" />{:else if detail}
        <p class="trellis-metadata">Installed revision {String(detail.revision)} · Package <code>{detail.packageDigest}</code></p>
        <div class="flex flex-wrap gap-3 items-end">
          <label class="form-control min-w-64"><span class="trellis-field-label">Apply permission preset</span><select class="select select-bordered select-sm mt-1" disabled={pending || !!catalogError} value="" onchange={(event) => applyPreset(event.currentTarget.value)}><option value="">Choose a capability group</option>{#each groups as group (group.groupKey)}<option value={group.groupKey}>{group.displayName} ({group.groupKey})</option>{/each}</select></label>
          <p class="trellis-field-help max-w-xl">Presets add the group's current permissions. Future group changes do not update this grant.</p>
        </div>
        {#if permissionCapabilities.length}
          <fieldset disabled={pending} class="space-y-2"><legend class="trellis-field-label mb-2">Capabilities</legend>
            {#each permissionCapabilities as capability (capability.capability)}
              <label class="flex gap-3 items-start"><input type="checkbox" class="checkbox checkbox-sm mt-0.5" checked={capability.allows.every((permission) => selected.has(permissionKey(permission)))} onchange={(event) => { for (const permission of capability.allows) { if (event.currentTarget.checked) selected.add(permissionKey(permission)); else selected.delete(permissionKey(permission)); } review = false; }} />
                <span><span class="text-sm font-medium">{capability.displayName || capability.capability}</span><span class="trellis-field-help block">{capability.description}</span><code class="trellis-metadata">{capability.capability}</code></span>
              </label>
            {/each}
          </fieldset>
        {/if}
        <fieldset disabled={pending} class="space-y-2"><legend class="trellis-field-label mb-2">Exact application permissions</legend>
          {#each available as permission (permissionKey(permission))}
            <label class="flex gap-3 items-start"><input class="checkbox checkbox-sm mt-0.5" type="checkbox" checked={selected.has(permissionKey(permission))} onchange={(event) => { if (event.currentTarget.checked) selected.add(permissionKey(permission)); else selected.delete(permissionKey(permission)); review = false; }} /><span class="text-xs break-words">{permissionLabel(permission)}<span class="trellis-metadata block">{detail.requiredGrants.permissions.some((entry) => permissionKey(entry) === permissionKey(permission)) ? "Required by application; removing this may prevent it from connecting" : detail.optionalBundles.some((bundle) => bundle.permissions.some((entry) => permissionKey(entry) === permissionKey(permission))) ? "Optional" : "Existing permission outside current catalog, preserved until explicitly removed"}</span></span></label>
          {:else}<p class="trellis-field-help">This participant declares no application permissions.</p>{/each}
        </fieldset>
        <div class="border-t border-base-300 pt-3 space-y-3">
          <label class="form-control max-w-sm"><span class="trellis-field-label">Expires at (local time)</span><input type="datetime-local" class="input input-bordered input-sm mt-1" bind:value={expiry} disabled={pending} onchange={() => review = false} /><span class="trellis-field-help">Leave empty for no expiry.</span></label>
          <label class="flex gap-3 items-start"><input type="checkbox" class="checkbox checkbox-sm mt-0.5" bind:checked={platformAdmin} disabled={pending} onchange={() => review = false} /><span><span class="text-sm font-medium">Trellis platform administration</span><span class="trellis-field-help block">Separate from application permissions. Grants platform-administrator authority.</span></span></label>
        </div>
        <section class="border-t border-base-300 pt-3 space-y-2" aria-label="Access change review">
          <h4 class="text-sm font-semibold">Changes to {ownerLabel}'s access</h4>
          {#each added as permission (permissionKey(permission))}<p class="text-xs"><span class="font-medium text-success">Add:</span> {permissionLabel(permission)}</p>{/each}
          {#each removed as permission (permissionKey(permission))}<p class="text-xs"><span class="font-medium text-error">Remove:</span> {permissionLabel(permission)}</p>{/each}
          {#if !added.length && !removed.length}<p class="trellis-field-help">Application permissions unchanged.</p>{/if}
          <p class="trellis-field-help">Platform administration: {platformAdmin ? "granted" : "not granted"}. Expiry: {expiry || "never"}.</p>
          {#if original && original.approvalMode !== "exact"}<Notice variant="warning">Saving replaces capability approval with an administrator's exact permission snapshot.</Notice>{/if}
          {#if ownerKind === "user" && ownerId === authority.identity.principalId}<Notice variant="warning">You are editing your own access. Changes may end this connection or remove your ability to administer Trellis.</Notice>{/if}
          {#if ownerKind === "deployment"}<Notice variant="warning">This changes deployment access for its instances. Companion or delegated access is a separate relationship.</Notice>{/if}
          <label class="flex items-center gap-3 text-sm"><input class="checkbox checkbox-sm" type="checkbox" bind:checked={review} disabled={pending} />I reviewed these access changes.</label>
          <button class="btn btn-outline btn-sm" type="button" disabled={busy || !review || !!editorError || !!catalogError || !canPerform(authority.authority, "grantsSet")} onclick={() => void saveAccess()}>{pending ? "Saving access…" : "Save access"}</button>
        </section>
      {/if}
    </section>
  {/if}
</Panel>
