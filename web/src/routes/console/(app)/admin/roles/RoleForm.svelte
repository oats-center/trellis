<script lang="ts">
  import { apis } from "$lib/trellis-apis";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { onDestroy, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { resolve } from "$lib/console_paths";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import {
    captureIntent,
    classifyMutationError,
    MutationController,
  } from "$lib/console/mutation.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import {
    draftConflicts,
    rebaseDraft,
    settleConflict,
  } from "$lib/console/draft.ts";
  import DraftConflicts from "$lib/components/DraftConflicts.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import PermissionGroups from "$lib/components/PermissionGroups.svelte";

  type Role = apis.auth.RolesGetOutput["role"];
  type Fields = {
    title: string;
    description: string;
    forNewUsers: boolean;
    selected: string[];
  };
  const fieldsOf = (role: Role | null): Fields => ({
    title: role?.title ?? "",
    description: role?.description ?? "",
    forNewUsers: role?.assignToNewUsers ?? false,
    selected: (role?.capabilities ?? [])
      .map((capability) => capability.capabilityId)
      .sort(),
  });
  const LABELS: Record<keyof Fields, string> = {
    title: "Title",
    description: "Description",
    forNewUsers: "Give to new users",
    selected: "Permissions",
  };

  let { roleId = null }: { roleId?: string | null } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("role");
  const catalogScope = new RequestScope("role-capabilities");
  const mutation = new MutationController<
    apis.auth.RolesPutInput,
    apis.auth.RolesPutOutput
  >();
  let role = $state.raw<Role | null>(null);
  let capabilities = $state.raw<apis.auth.CapabilitiesListOutput["items"]>([]);
  let title = $state("");
  let description = $state("");
  let forNewUsers = $state(false);
  let selected = $state<string[]>([]);
  // The saved role each edit was typed over; see $lib/console/draft.ts.
  let base = $state.raw<Fields>(fieldsOf(null));
  let loading = $state(true);
  let saving = $state(false);
  // A failed read, or a change whose outcome is unknown; both block saving.
  let error = $state<string | null>(null);
  // Why Auth refused the last change. The form rereads and stays usable.
  let refusal = $state<string | null>(null);
  let catalogError = $state<string | null>(null);
  const draft = (): Fields => ({
    title,
    description,
    forNewUsers,
    selected: [...selected].sort(),
  });
  const conflicts = $derived(
    role ? draftConflicts({ draft: draft(), base }, fieldsOf(role)) : [],
  );
  const unknown = $derived(
    selected.filter(
      (id) =>
        !capabilities.some(
          (capability) => capability.identity.capabilityId === id,
        ),
    ),
  );

  function show(fields: Fields): void {
    ({ title, description, forNewUsers, selected } = fields);
  }

  function settle(field: string, choice: "keep" | "take"): void {
    if (!role || !(field in LABELS)) return;
    const settled = settleConflict(
      { draft: draft(), base },
      fieldsOf(role),
      field as keyof Fields,
      choice,
    );
    show(settled.draft);
    base = settled.base;
  }

  function describe(field: keyof Fields, fields: Fields): string {
    if (field === "forNewUsers") return fields.forNewUsers ? "Yes" : "No";
    if (field === "selected")
      return fields.selected.length === 0
        ? "None"
        : fields.selected
            .map(
              (id) =>
                capabilities.find(
                  (capability) => capability.identity.capabilityId === id,
                )?.title ?? id,
            )
            .join(", ");
    return fields[field] || "(empty)";
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    // A reread of the shown role keeps the form and what was typed in it.
    const reread = role !== null && role.roleId === roleId;
    if (!reread) {
      loading = true;
      role = null;
      show(fieldsOf(null));
      base = fieldsOf(null);
    }
    error = null;
    try {
      if (roleId) {
        const input = apis.auth.API.actions["rpc:Roles.Get"].input.decode({
          roleId,
        });
        const response = await trellis.rolesGet(input).orThrow();
        if (!scope.isCurrent(token)) return;
        const saved = fieldsOf(response.role);
        if (reread && role?.roleId === response.role.roleId) {
          const merged = rebaseDraft({ draft: draft(), base }, saved);
          show(merged.draft);
          base = merged.base;
        } else {
          show(saved);
          base = saved;
        }
        role = response.role;
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
      conflicts.length > 0 ||
      (roleId && !role) ||
      !canPerform(authority.authority, "rolesPut")
    )
      return;
    const key = codecs.ulid.decode(ulid());
    const input: apis.auth.RolesPutInput = {
      ...(role ? { roleId: role.roleId } : {}),
      expectedRevision: role?.revision ?? 0n,
      title: title.trim(),
      description: description.trim(),
      capabilityIds: [...selected],
      assignToNewUsers: forNewUsers,
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
    refusal = null;
    const outcome = await mutation.send({
      isStillValid: () =>
        scope.key === intent.scope.routeKey &&
        (role?.revision ?? 0n) === input.expectedRevision,
      dispatch: async ({ input }) => await trellis.rolesPut(input).orThrow(),
    });
    saving = false;
    if (!outcome || scope.key !== intent.scope.routeKey) return;
    if (outcome.kind === "succeeded") await goto(resolve("/admin/roles"));
    else if (outcome.kind === "unknown")
      error = "Outcome unknown; reload the role before another change.";
    else {
      const message = projectConsoleError(outcome.error).message;
      // A refused save of a role usually means someone changed it meanwhile;
      // reread it so the form shows what they saved beside what was typed.
      if (role) await load();
      if (scope.key === intent.scope.routeKey) refusal = message;
    }
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
    refusal = null;
    try {
      await trellis
        .rolesDelete({
          roleId: role.roleId,
          expectedRevision: role.revision,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (scope.key === key) await goto(resolve("/admin/roles"));
    } catch (cause) {
      if (scope.key !== key) return;
      const message = projectConsoleError(cause).message;
      // A refused delete means the role changed since it was read; show it as
      // it is now, keeping what was typed.
      if (classifyMutationError(cause).kind === "unknown") error = message;
      else {
        await load();
        if (scope.key === key) refusal = message;
      }
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

<PageToolbar
  eyebrow="Roles"
  title={roleId ? (role?.title ?? "Edit role") : "New role"}
  description={roleId
    ? undefined
    : "Choose a name and what users with it can do."}
/>

{#if error}<Notice variant="error"
    >{error}<button
      class="btn btn-ghost btn-xs ml-2"
      onclick={() => void load()}>Reload role</button
    ></Notice
  >{/if}
{#if refusal}<Notice variant="error">{refusal}</Notice>{/if}
{#if loading}<LoadingState label="Loading role" />{:else if !roleId || role}
  <form
    class="max-w-3xl space-y-5"
    onsubmit={(event) => {
      event.preventDefault();
      void save();
    }}
  >
    <div class="trellis-section space-y-4 p-5">
      <label class="trellis-field"
        ><span class="trellis-field-label">Title</span><input
          class="input w-full"
          bind:value={title}
          required
          disabled={saving}
        /></label
      >
      <label class="trellis-field"
        ><span class="trellis-field-label">Description</span><input
          class="input w-full"
          bind:value={description}
          placeholder="What users with this role do"
          disabled={saving}
        /></label
      >
      <label class="flex items-start gap-3"
        ><input
          class="checkbox checkbox-sm mt-0.5"
          type="checkbox"
          bind:checked={forNewUsers}
          disabled={saving}
        /><span
          ><span class="font-medium">Give to new users</span><span
            class="block text-sm text-base-content/65"
            >Everyone added from now on starts with this role. Users who already
            have accounts keep the roles they have.</span
          ></span
        ></label
      >
    </div>
    <div class="trellis-section space-y-3 p-5">
      <div>
        <h2 class="text-base font-semibold">Permissions</h2>
        <p class="text-sm text-base-content/65">
          What users with this role can do.
        </p>
      </div>
      {#if catalogError}<Notice variant="warning"
          >Some permissions couldn't be loaded: {catalogError}</Notice
        >{/if}
      <PermissionGroups {capabilities} bind:selected disabled={saving} />
      {#if unknown.length}
        <div class="rounded-field bg-base-200/60 p-3 text-sm">
          <p class="font-medium">Permissions that are no longer installed</p>
          <ul class="mt-1 space-y-1">
            {#each unknown as id (id)}<li class="flex items-center gap-2">
                <input
                  class="checkbox checkbox-sm"
                  type="checkbox"
                  checked={true}
                  aria-label={`Keep ${id}`}
                  disabled={saving}
                  onchange={(event) => select(id, event.currentTarget.checked)}
                /><span class="trellis-identifier">{id}</span>
              </li>{/each}
          </ul>
        </div>
      {/if}
    </div>
    {#if role && conflicts.length > 0}<DraftConflicts
        conflicts={conflicts.map((field) => ({
          field,
          label: LABELS[field],
          mine: describe(field, draft()),
          saved: describe(field, fieldsOf(role)),
        }))}
        disabled={saving}
        onsettle={settle}
      />{/if}
    <div class="flex justify-between gap-2">
      <div>
        {#if role}<button
            class="btn btn-error btn-outline"
            type="button"
            disabled={saving || !canPerform(authority.authority, "rolesDelete")}
            onclick={() => void remove()}>Delete role</button
          >{/if}
      </div>
      <div class="flex gap-2">
        <a class="btn btn-ghost" href={resolve("/admin/roles")}>Cancel</a
        ><button
          class="btn btn-primary"
          type="submit"
          disabled={saving ||
            !!error ||
            conflicts.length > 0 ||
            !canPerform(authority.authority, "rolesPut")}
          >{saving ? "Saving…" : "Save role"}</button
        >
      </div>
    </div>
  </form>
{/if}
