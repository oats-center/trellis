<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onDestroy, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { captureIntent, MutationController } from "$lib/console/mutation.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ChoiceRow from "$lib/components/ChoiceRow.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Panel from "$lib/components/Panel.svelte";

  let { roleId = null }: { roleId?: string | null } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("role");
  const catalogScope = new RequestScope("role-capabilities");
  const mutation = new MutationController<
    apis.auth.RolesPutInput,
    apis.auth.RolesPutOutput
  >();
  let role = $state.raw<apis.auth.RolesGetOutput["role"] | null>(null);
  let capabilities = $state.raw<apis.auth.CapabilitiesListOutput["items"]>([]);
  let title = $state("");
  let description = $state("");
  let selected = $state<string[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let catalogError = $state<string | null>(null);
  const unknown = $derived(
    selected.filter(
      (id) =>
        !capabilities.some(
          (capability) => capability.identity.capabilityId === id,
        ),
    ),
  );

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    role = null;
    title = "";
    description = "";
    selected = [];
    try {
      if (roleId) {
        const input = apis.auth.API.actions["rpc:Roles.Get"].input.decode({
          roleId,
        });
        const response = await trellis.rolesGet(input).orThrow();
        if (!scope.isCurrent(token)) return;
        role = response.role;
        title = role.title;
        description = role.description;
        selected = role.capabilities.map(
          (capability) => capability.capabilityId,
        );
      }
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function loadCapabilities(): Promise<void> {
    const token = catalogScope.begin();
    const result = await traverseAll(async (page) => {
      const response = await trellis.capabilitiesList({ page }).orThrow();
      return { items: response.items, cursor: response.page.nextCursor };
    });
    if (!catalogScope.settle(token)) return;
    capabilities = [...result.items];
    catalogError = result.complete
      ? null
      : projectConsoleError(result.error).message;
  }

  function select(id: string, checked: boolean): void {
    selected = checked
      ? [...new Set([...selected, id])]
      : selected.filter((value) => value !== id);
  }

  async function save(): Promise<void> {
    if (
      loading ||
      saving ||
      error ||
      (roleId && !role) ||
      !canPerform(authority.authority, "rolesPut")
    )
      return;
    const key = ulid();
    const input: apis.auth.RolesPutInput = {
      ...(role ? { roleId: role.roleId } : {}),
      expectedRevision: role?.revision ?? 0n,
      title: title.trim(),
      description: description.trim(),
      capabilityIds: [...selected],
      idempotencyKey: key,
    };
    const intent = captureIntent({
      operation: "rolesPut",
      targetId: role?.roleId ?? "new",
      label: input.title,
      idempotencyKey: key,
      input,
      scope: { routeKey: scope.key },
    });
    if (!mutation.begin(intent)) return;
    saving = true;
    const outcome = await mutation.send({
      isStillValid: () =>
        scope.key === intent.scope.routeKey &&
        (role?.revision ?? 0n) === input.expectedRevision,
      dispatch: async ({ input }) => await trellis.rolesPut(input).orThrow(),
    });
    saving = false;
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") await goto(resolve("/admin/roles"));
    else
      error =
        outcome.kind === "unknown"
          ? "Outcome unknown; reload the role before another change."
          : projectConsoleError(outcome.error).message;
  }

  async function remove(): Promise<void> {
    if (
      !role ||
      saving ||
      !canPerform(authority.authority, "rolesDelete") ||
      !window.confirm(`Delete role ${role.title}?`)
    )
      return;
    const key = scope.key;
    saving = true;
    try {
      await trellis
        .rolesDelete({
          roleId: role.roleId,
          expectedRevision: role.revision,
          idempotencyKey: ulid(),
        })
        .orThrow();
      if (scope.key === key) await goto(resolve("/admin/roles"));
    } catch (cause) {
      if (scope.key === key) error = projectConsoleError(cause).message;
    } finally {
      saving = false;
    }
  }

  $effect(() => {
    scope.setKey(roleId ?? "new");
    mutation.cancel();
    untrack(() => {
      void load();
      void loadCapabilities();
    });
    return () => {
      scope.invalidate();
      catalogScope.invalidate();
    };
  });
  onDestroy(() => {
    scope.dispose();
    catalogScope.dispose();
  });
</script>

{#if error}<Notice variant="error"
    >{error}<button
      class="btn btn-ghost btn-xs ml-2"
      onclick={() => void load()}>Reload role</button
    ></Notice
  >{/if}
{#if loading}<LoadingState label="Loading role" />{:else if !roleId || role}
  <form
    class="space-y-4"
    onsubmit={(event) => {
      event.preventDefault();
      void save();
    }}
  >
    <Panel title="Role">
      <p class="trellis-field-help">
        A role contains whole capabilities, never individual action-level
        grants.
      </p>
      <div class="grid gap-3 p-4 md:grid-cols-2">
        <label class="form-control"
          ><span class="trellis-field-label">Title</span><input
            class="input input-bordered input-sm"
            bind:value={title}
            required
            disabled={saving}
          /></label
        >
        <label class="form-control"
          ><span class="trellis-field-label">Description</span><input
            class="input input-bordered input-sm"
            bind:value={description}
            disabled={saving}
          /></label
        >
      </div>
      {#if role}<p class="trellis-metadata px-4 pb-3">
          {role.roleId} · Identity generation {role.identityGeneration} · Revision
          {role.revision}
        </p>{/if}
    </Panel>
    <Panel title="Capabilities">
      <p class="trellis-field-help mb-3">
        Select complete, accepted capabilities. Existing selections survive an
        incomplete discovery catalog.
      </p>
      {#if catalogError}<Notice variant="warning"
          >Partial catalog: {catalogError}</Notice
        >{/if}
      {#each capabilities as capability (capability.identity.capabilityId)}
        <ChoiceRow>
          {#snippet input()}<input
              class="checkbox checkbox-xs"
              type="checkbox"
              checked={selected.includes(capability.identity.capabilityId)}
              disabled={saving}
              onchange={(event) =>
                select(
                  capability.identity.capabilityId,
                  event.currentTarget.checked,
                )}
            />{/snippet}
          <span class="text-sm font-medium">{capability.title}</span>
          <p class="trellis-identifier">{capability.identity.capabilityId}</p>
          <p class="trellis-field-help">{capability.description}</p>
        </ChoiceRow>
      {/each}
      {#each unknown as id (id)}<ChoiceRow
          >{#snippet input()}<input
              class="checkbox checkbox-xs"
              type="checkbox"
              checked={true}
              disabled={saving}
              onchange={(event) => select(id, event.currentTarget.checked)}
            />{/snippet}<span class="trellis-identifier">{id}</span>
          <p class="trellis-field-help">
            Existing selection not present in the loaded catalog.
          </p></ChoiceRow
        >{/each}
    </Panel>
    <div class="flex justify-between gap-2">
      <div>
        {#if role}<button
            class="btn btn-error btn-outline btn-sm"
            type="button"
            disabled={saving || !canPerform(authority.authority, "rolesDelete")}
            onclick={() => void remove()}>Delete role</button
          >{/if}
      </div>
      <div class="flex gap-2">
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/roles")}>Cancel</a
        ><button
          class="btn btn-outline btn-sm"
          type="submit"
          disabled={saving ||
            !!error ||
            !canPerform(authority.authority, "rolesPut")}
          >{saving ? "Saving…" : "Save role"}</button
        >
      </div>
    </div>
  </form>
{/if}
