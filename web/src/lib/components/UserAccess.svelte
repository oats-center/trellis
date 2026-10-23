<script lang="ts">
  import {
    capabilityArea,
    groupByArea,
  } from "$lib/console/capability_areas.ts";
  import { onDestroy, untrack } from "svelte";
  import { ulid } from "ulid";
  import { apis } from "trellis-web-generated";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { formatDate } from "$lib/format.ts";
  import Panel from "./Panel.svelte";
  import Notice from "./Notice.svelte";
  import LoadingState from "./LoadingState.svelte";

  let { principalId }: { principalId: apis.auth.UsersGetInput["userId"] } =
    $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("user-access");
  let grants = $state.raw<
    readonly apis.auth.CapabilityGrantsListOutput["items"][number][]
  >([]);
  let assignments = $state.raw<
    readonly apis.auth.RoleAssignmentsListOutput["items"][number][]
  >([]);
  let capabilities = $state.raw<
    readonly apis.auth.CapabilitiesListOutput["items"][number][]
  >([]);
  let roles = $state.raw<readonly apis.auth.RolesListOutput["items"][number][]>(
    [],
  );
  let capabilityId = $state("");
  let roleId = $state("");
  let expiresAt = $state("");
  let loading = $state(true);
  let busy = $state(false);
  let uncertain = $state(false);
  let error = $state<string | null>(null);

  async function load(): Promise<void> {
    const id = principalId;
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const results = await Promise.all([
        traverseAll(async (page) => {
          if (!scope.isCurrent(token))
            throw new Error("Access read superseded.");
          const response = await trellis
            .capabilityGrantsList({ principalId: id, page })
            .orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        traverseAll(async (page) => {
          if (!scope.isCurrent(token))
            throw new Error("Access read superseded.");
          const response = await trellis
            .roleAssignmentsList({ principalId: id, page })
            .orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        traverseAll(async (page) => {
          if (!scope.isCurrent(token))
            throw new Error("Access read superseded.");
          const response = await trellis.capabilitiesList({ page }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
        traverseAll(async (page) => {
          if (!scope.isCurrent(token))
            throw new Error("Access read superseded.");
          const response = await trellis.rolesList({ page }).orThrow();
          return { items: response.items, cursor: response.page.nextCursor };
        }),
      ]);
      if (!scope.isCurrent(token)) return;
      const failed = results.find((result) => !result.complete);
      if (failed) throw failed.error;
      grants = results[0].items;
      assignments = results[1].items;
      capabilities = results[2].items;
      roles = results[3].items;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  function roleTitle(id: string): string {
    return roles.find((role) => role.roleId === id)?.title ?? "Unknown role";
  }

  function capabilityTitle(id: string): string {
    return (
      capabilities.find((item) => item.identity.capabilityId === id)?.title ??
      id
    );
  }

  function label(
    existing: (typeof assignments)[number] | (typeof grants)[number],
  ): string {
    // Look the record up in its own typed list rather than narrowing the union.
    const assignment = assignments.find((item) => item === existing);
    if (assignment) return `the ${roleTitle(assignment.roleId)} role`;
    const grant = grants.find((item) => item === existing);
    return grant
      ? capabilityTitle(grant.capability.capabilityId)
      : "this access";
  }

  // Grantable permissions grouped by API area, as in the role picker.
  const grantable = $derived(
    groupByArea(
      capabilities.filter(
        (item) =>
          !grants.some(
            (grant) =>
              grant.capability.capabilityId === item.identity.capabilityId,
          ),
      ),
      (item) => item.identity.capabilityId,
    ),
  );

  async function change(
    kind: "role" | "capability",
    existing:
      | (typeof assignments)[number]
      | (typeof grants)[number]
      | undefined = undefined,
  ): Promise<void> {
    const operation =
      kind === "role"
        ? existing
          ? "roleAssignmentsRevoke"
          : "roleAssignmentsAssign"
        : existing
          ? "capabilityGrantsRevoke"
          : "capabilityGrantsGrant";
    if (
      loading ||
      busy ||
      uncertain ||
      error ||
      !canPerform(authority.authority, operation)
    )
      return;
    const id = principalId;
    const route = scope.key;
    const selected = kind === "role" ? roleId : capabilityId;
    if (!existing && !selected) return;
    if (
      !window.confirm(
        existing
          ? `Remove ${label(existing)}?`
          : `Give ${kind === "role" ? `the ${roleTitle(selected)} role` : capabilityTitle(selected)}?`,
      )
    )
      return;
    busy = true;
    error = null;
    const key = ulid();
    try {
      const expiry =
        !existing && expiresAt
          ? { expiresAt: new Date(expiresAt).toISOString() }
          : {};
      if (kind === "role") {
        if (existing && "assignmentId" in existing) {
          await trellis
            .roleAssignmentsRevoke(
              apis.auth.API.actions["rpc:RoleAssignments.Revoke"].input.decode({
                assignmentId: existing.assignmentId,
                expectedRevision: existing.revision,
                idempotencyKey: key,
              }),
            )
            .orThrow();
        } else if (!existing) {
          await trellis
            .roleAssignmentsAssign(
              apis.auth.API.actions["rpc:RoleAssignments.Assign"].input.decode({
                principalId: id,
                roleId: selected,
                expectedRevision: 0n,
                idempotencyKey: key,
                ...expiry,
              }),
            )
            .orThrow();
        }
      } else if (existing && "grantId" in existing) {
        await trellis
          .capabilityGrantsRevoke(
            apis.auth.API.actions["rpc:CapabilityGrants.Revoke"].input.decode({
              grantId: existing.grantId,
              expectedRevision: existing.revision,
              idempotencyKey: key,
            }),
          )
          .orThrow();
      } else if (!existing) {
        await trellis
          .capabilityGrantsGrant(
            apis.auth.API.actions["rpc:CapabilityGrants.Grant"].input.decode({
              principalId: id,
              capabilityId: selected,
              expectedRevision: 0n,
              idempotencyKey: key,
              ...expiry,
            }),
          )
          .orThrow();
      }
      if (scope.disposed || scope.key !== route) return;
      capabilityId = "";
      roleId = "";
      await load();
    } catch (cause) {
      if (scope.disposed || scope.key !== route) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "We couldn't confirm the change. Refresh before trying again."
        : projectConsoleError(cause).message;
    } finally {
      if (!scope.disposed && scope.key === route) busy = false;
    }
  }

  $effect(() => {
    const targetChanged = scope.key !== `user-access:${principalId}`;
    void authority.snapshot.generation;
    scope.setKey(`user-access:${principalId}`);
    untrack(() => {
      grants = [];
      assignments = [];
      capabilities = [];
      roles = [];
      capabilityId = "";
      roleId = "";
      if (targetChanged) {
        expiresAt = "";
        uncertain = false;
        busy = false;
      }
      void load();
    });
    return () => scope.invalidate();
  });
  onDestroy(() => scope.dispose());
</script>

<Panel>
  <div class="mb-4 flex items-start justify-between gap-3">
    <div>
      <h2 class="text-base font-semibold">Access</h2>
      <p class="text-sm text-base-content/65">
        What this person can do. Apps still ask them before using it.
      </p>
    </div>
    <button
      class="btn btn-ghost btn-sm"
      disabled={loading || busy}
      onclick={async () => {
        await load();
        if (!error) uncertain = false;
      }}>Refresh</button
    >
  </div>
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if loading}<LoadingState label="Loading user access" />{:else}
    <h3 class="text-sm font-semibold">Roles</h3>
    <ul
      class="mt-2 divide-y divide-base-300 rounded-field border border-base-300"
    >
      {#each assignments as assignment (assignment.assignmentId)}
        <li class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span>
            <span class="font-medium">{roleTitle(assignment.roleId)}</span>
            {#if assignment.expiresAt}<span class="text-base-content/60">
                · until {formatDate(assignment.expiresAt)}</span
              >{/if}
          </span>
          <button
            class="btn btn-ghost btn-xs text-error"
            onclick={() => change("role", assignment)}
            disabled={busy ||
              uncertain ||
              !!error ||
              !canPerform(authority.authority, "roleAssignmentsRevoke")}
            >Remove</button
          >
        </li>
      {:else}
        <li class="px-3 py-2 text-sm text-base-content/60">No roles yet.</li>
      {/each}
    </ul>
    <div class="mt-2 flex flex-wrap gap-2">
      <select
        class="select select-sm flex-1"
        aria-label="Role to give"
        bind:value={roleId}
        disabled={busy || uncertain || !!error}
      >
        <option value="">Choose a role…</option>
        {#each roles.filter((item) => !assignments.some((assignment) => assignment.roleId === item.roleId)) as item (item.roleId)}<option
            value={item.roleId}>{item.title}</option
          >{/each}
      </select>
      <button
        class="btn btn-outline btn-sm"
        onclick={() => change("role")}
        disabled={!roleId ||
          busy ||
          uncertain ||
          !!error ||
          !canPerform(authority.authority, "roleAssignmentsAssign")}
        >Give role</button
      >
    </div>

    <h3 class="mt-6 text-sm font-semibold">Extra permissions</h3>
    <p class="text-xs text-base-content/60">
      One-off permissions outside of roles.
    </p>
    <ul
      class="mt-2 divide-y divide-base-300 rounded-field border border-base-300"
    >
      {#each grants as grant (grant.grantId)}
        {@const meaning = capabilities.find(
          (item) =>
            item.identity.capabilityId === grant.capability.capabilityId,
        )}
        <li class="flex items-center justify-between gap-3 px-3 py-2 text-sm">
          <span>
            <span class="font-medium"
              >{meaning?.title ?? grant.capability.capabilityId}</span
            >
            <span class="text-base-content/60"
              >{meaning
                ? `· ${capabilityArea(meaning.identity.capabilityId).title}`
                : ""}{grant.expiresAt
                ? ` · until ${formatDate(grant.expiresAt)}`
                : ""}</span
            >
          </span>
          <button
            class="btn btn-ghost btn-xs text-error"
            onclick={() => change("capability", grant)}
            disabled={busy ||
              uncertain ||
              !!error ||
              !canPerform(authority.authority, "capabilityGrantsRevoke")}
            >Remove</button
          >
        </li>
      {:else}
        <li class="px-3 py-2 text-sm text-base-content/60">None.</li>
      {/each}
    </ul>
    <div class="mt-2 flex flex-wrap gap-2">
      <select
        class="select select-sm flex-1"
        aria-label="Permission to give"
        bind:value={capabilityId}
        disabled={busy || uncertain || !!error}
      >
        <option value="">Choose a permission…</option>
        {#each grantable as group (group.id)}
          <optgroup label={group.title}>
            {#each group.items as item (item.identity.capabilityId)}<option
                value={item.identity.capabilityId}>{item.title}</option
              >{/each}
          </optgroup>
        {/each}
      </select>
      <button
        class="btn btn-outline btn-sm"
        onclick={() => change("capability")}
        disabled={!capabilityId ||
          busy ||
          uncertain ||
          !!error ||
          !canPerform(authority.authority, "capabilityGrantsGrant")}
        >Give permission</button
      >
    </div>

    <label class="trellis-field mt-5 max-w-xs"
      ><span class="trellis-field-label">Until (optional)</span><input
        class="input input-sm"
        type="datetime-local"
        bind:value={expiresAt}
        disabled={busy || uncertain || !!error}
      /><span class="trellis-field-help"
        >New roles and permissions end at this time.</span
      ></label
    >
  {/if}
</Panel>
