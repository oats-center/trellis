<script lang="ts">
  import { onMount } from "svelte";
  import type { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { isUncertainError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import Notice from "$lib/components/Notice.svelte";

  type Mapping = apis.auth.OIDCRoleMappingsListOutput["items"][number];
  type Role = apis.auth.RolesListOutput["items"][number];

  /** Roles people get from the groups this provider says they are in. */
  let {
    providerId,
    claim,
  }: {
    providerId: Mapping["providerId"];
    /** Where the provider lists groups; empty means it gives none. */
    claim: string;
  } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  let mappings = $state.raw<Mapping[]>([]);
  let roles = $state.raw<Role[]>([]);
  let group = $state("");
  let chosen = $state<Role["roleId"][]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const editable = $derived(
    canPerform(authority.authority, "oidcRoleMappingsPut"),
  );
  const titles = $derived(
    new Map(roles.map((role) => [role.roleId, role.title])),
  );

  async function load(): Promise<void> {
    const [listed, available] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis
          .oidcRoleMappingsList({ providerId, page })
          .orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.rolesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    mappings = [...listed.items];
    roles = [...available.items];
  }

  async function run(work: () => Promise<void>): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    try {
      await work();
      await load();
    } catch (cause) {
      error = isUncertainError(cause)
        ? "We couldn't confirm the change. Reload before trying again."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  function add(): Promise<void> {
    return run(async () => {
      await trellis
        .oidcRoleMappingsPut({
          providerId,
          externalRole: group.trim(),
          roleIds: chosen,
          expectedRevision: 0n,
          idempotencyKey: ulid(),
        })
        .orThrow();
      group = "";
      chosen = [];
    });
  }

  function remove(mapping: Mapping): Promise<void> {
    return run(async () => {
      await trellis
        .oidcRoleMappingsDelete({
          mappingId: mapping.mappingId,
          expectedRevision: mapping.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
    });
  }

  onMount(() => {
    load().catch((cause) => (error = projectConsoleError(cause).message));
  });
</script>

<div class="space-y-3">
  <div>
    <span class="trellis-field-label">Roles from groups</span>
    <p class="trellis-field-help">
      People in a group at this provider get these roles while they sign in with
      it.
    </p>
  </div>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if mappings.length}
    <ul class="divide-y divide-base-300 rounded border border-base-300">
      {#each mappings as mapping (mapping.mappingId)}
        <li class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span
            ><span class="font-mono">{mapping.externalRole}</span> →
            {mapping.roleIds
              .map((roleId) => titles.get(roleId) ?? "Removed role")
              .join(", ")}</span
          >
          {#if canPerform(authority.authority, "oidcRoleMappingsDelete")}<button
              type="button"
              class="btn btn-ghost btn-xs"
              disabled={busy}
              aria-label={`Remove ${mapping.externalRole}`}
              onclick={() => void remove(mapping)}>Remove</button
            >{/if}
        </li>
      {/each}
    </ul>
  {/if}
  {#if !claim}
    <p class="text-sm">Set a groups claim under More settings to give roles.</p>
  {:else if editable}
    <div class="space-y-2 rounded border border-base-300 p-3">
      <label class="trellis-field"
        ><span class="trellis-field-label">Group</span><input
          class="input input-sm w-full font-mono"
          bind:value={group}
          placeholder="engineering"
        /></label
      >
      <fieldset class="space-y-1">
        <legend class="trellis-field-label">Roles</legend>
        {#each roles as role (role.roleId)}
          <label class="flex items-center gap-2 text-sm"
            ><input
              class="checkbox checkbox-sm"
              type="checkbox"
              value={role.roleId}
              bind:group={chosen}
            />{role.title}</label
          >
        {/each}
      </fieldset>
      <button
        type="button"
        class="btn btn-sm"
        disabled={busy || !group.trim() || chosen.length === 0}
        onclick={() => void add()}>Add group</button
      >
    </div>
  {/if}
</div>
