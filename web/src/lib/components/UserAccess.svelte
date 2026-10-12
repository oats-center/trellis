<script lang="ts">
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
  import DataTable from "./DataTable.svelte";
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
          ? `Revoke ${kind} assignment ${kind === "role" ? existing.assignmentId : existing.grantId} from user ${id}?`
          : `Assign complete ${kind} ${selected} to user ${id}?`,
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
        ? "Access change outcome unknown. Refresh and inspect the assignments before another change."
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
  <div class="mb-3 flex items-center justify-between gap-3">
    <div>
      <h2
        class="text-sm font-semibold uppercase tracking-wide text-base-content/70"
      >
        Access
      </h2>
      <p class="text-xs text-base-content/50">
        Grant complete installed capabilities or roles. OAuth applications still
        require separate user consent.
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
    <label class="form-control mb-3 max-w-sm"
      ><span class="label-text mb-1 text-xs text-base-content/60"
        >Expiry for new assignments (optional, local time)</span
      ><input
        class="input input-bordered input-sm"
        type="datetime-local"
        bind:value={expiresAt}
        disabled={busy || uncertain || !!error}
      /></label
    >
    <h3 class="mb-2 text-sm font-semibold">Capabilities</h3>
    <div class="mb-3 flex flex-wrap gap-2">
      <select
        class="select select-bordered select-sm flex-1"
        aria-label="Capability to grant"
        bind:value={capabilityId}
        disabled={busy || uncertain || !!error}
      >
        <option value="">Select a capability</option>
        {#each capabilities.filter((item) => !grants.some((grant) => grant.capability.capabilityId === item.identity.capabilityId)) as item (item.identity.capabilityId)}<option
            value={item.identity.capabilityId}
            >{item.title} · {item.identity.capabilityId}</option
          >{/each}
      </select>
      <button
        class="btn btn-primary btn-sm"
        onclick={() => change("capability")}
        disabled={!capabilityId ||
          busy ||
          uncertain ||
          !!error ||
          !canPerform(authority.authority, "capabilityGrantsGrant")}
        >Grant capability</button
      >
    </div>
    <DataTable
      ><thead
        ><tr><th>Capability</th><th>Generation</th><th>Expires</th><th></th></tr
        ></thead
      ><tbody>
        {#each grants as grant (grant.grantId)}<tr
            ><td class="trellis-identifier break-all"
              >{grant.capability.capabilityId}</td
            ><td>{grant.capability.identityGeneration}</td><td
              >{grant.expiresAt ? formatDate(grant.expiresAt) : "No expiry"}</td
            ><td
              ><button
                class="btn btn-ghost btn-xs text-error"
                onclick={() => change("capability", grant)}
                disabled={busy ||
                  uncertain ||
                  !!error ||
                  !canPerform(authority.authority, "capabilityGrantsRevoke")}
                >Revoke</button
              ></td
            ></tr
          >{:else}<tr
            ><td colspan="4" class="text-base-content/50"
              >No direct capability grants.</td
            ></tr
          >{/each}
      </tbody></DataTable
    >
    <h3 class="mb-2 mt-5 text-sm font-semibold">Roles</h3>
    <div class="mb-3 flex flex-wrap gap-2">
      <select
        class="select select-bordered select-sm flex-1"
        aria-label="Role to assign"
        bind:value={roleId}
        disabled={busy || uncertain || !!error}
      >
        <option value="">Select a role</option>
        {#each roles.filter((item) => !assignments.some((assignment) => assignment.roleId === item.roleId)) as item (item.roleId)}<option
            value={item.roleId}>{item.title}</option
          >{/each}
      </select>
      <button
        class="btn btn-primary btn-sm"
        onclick={() => change("role")}
        disabled={!roleId ||
          busy ||
          uncertain ||
          !!error ||
          !canPerform(authority.authority, "roleAssignmentsAssign")}
        >Assign role</button
      >
    </div>
    <DataTable
      ><thead
        ><tr><th>Role</th><th>Generation</th><th>Expires</th><th></th></tr
        ></thead
      ><tbody>
        {#each assignments as assignment (assignment.assignmentId)}<tr
            ><td
              ><span class="block"
                >{roles.find((role) => role.roleId === assignment.roleId)
                  ?.title ?? assignment.roleId}</span
              ><span class="trellis-identifier text-xs text-base-content/50"
                >{assignment.roleId}</span
              ></td
            ><td>{assignment.roleIdentityGeneration}</td><td
              >{assignment.expiresAt
                ? formatDate(assignment.expiresAt)
                : "No expiry"}</td
            ><td
              ><button
                class="btn btn-ghost btn-xs text-error"
                onclick={() => change("role", assignment)}
                disabled={busy ||
                  uncertain ||
                  !!error ||
                  !canPerform(authority.authority, "roleAssignmentsRevoke")}
                >Revoke</button
              ></td
            ></tr
          >{:else}<tr
            ><td colspan="4" class="text-base-content/50"
              >No role assignments.</td
            ></tr
          >{/each}
      </tbody></DataTable
    >
  {/if}
</Panel>
