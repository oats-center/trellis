<script lang="ts">
  import { isErr } from "@qlever-llc/result";
  import { type apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import {
    decodeKnownJsonBytes,
    displayJson,
    formatTimestamp,
    projectConsoleError,
  } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";
  import { getTrellis } from "$lib/trellis";

  type DeploymentDetail = apis.auth.DeploymentsGetOutput;

  const trellis = getTrellis();
  const RPC_TIMEOUT_MS = 10_000;

  // SvelteKit reuses this component when only the parameter changes, so the
  // exact deployment ID comes from page params and drives the read scope.
  const deploymentId = $derived(page.params.deploymentId ?? "");
  const scope = new RequestScope("");

  let loading = $state(true);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let detail = $state.raw<DeploymentDetail | null>(null);
  /** True when the current detail belongs to the current requested ID. */
  let loadedFor = $state<string | null>(null);
  /** True when the backend reported this exact deployment as absent. */
  let notFound = $state(false);

  function failureFrom(
    cause: unknown,
  ): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  async function load(): Promise<void> {
    const requestedId = deploymentId;
    const token = scope.begin();
    // A new target must not show the previous deployment's data.
    if (loadedFor !== requestedId) {
      detail = null;
      loadedFor = null;
      notFound = false;
    }
    loading = true;
    error = null;
    try {
      const response = await trellis.deploymentsGet({ deploymentId: requestedId }, {
        timeout: RPC_TIMEOUT_MS,
      }).take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = failureFrom(response);
        notFound = error.code === "not_found";
        return;
      }
      detail = response;
      loadedFor = requestedId;
      notFound = false;
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
      notFound = error.code === "not_found";
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  // One effect keyed by the semantic target: a parameter change clears the old
  // detail and starts a fresh read, and disposal invalidates late responses.
  $effect(() => {
    const requestedId = deploymentId;
    if (requestedId.length === 0) return;
    scope.setKey(requestedId);
    void load();
    return () => scope.invalidate();
  });

  const participantId = $derived(detail?.deployment.participantId ?? null);
  const binding = $derived(detail?.binding ?? null);
  const resources = $derived(detail?.resources ?? []);
  const resourceError = $derived(
    resources.find((resource) => resource.error)?.error ?? null,
  );

  function providerSummary(bytes: Uint8Array): string {
    const decoded = decodeKnownJsonBytes(bytes);
    if (!decoded.ok) return `Unreadable provider payload (${decoded.error})`;
    return displayJson(decoded.value);
  }

  function resourceStateVariant(state: string): "healthy" | "degraded" | "unhealthy" {
    if (state === "available") return "healthy";
    if (state === "stale") return "degraded";
    return "unhealthy";
  }
</script>

<PageToolbar
  title={detail?.deployment.displayName ?? (loadedFor ?? deploymentId) ?? "Deployment"}
  description="Current deployment, participant-scoped grant, and physical resource evidence."
>
  {#snippet actions()}
    <a class="btn btn-ghost btn-sm" href={resolve("/admin/services")}>Back to services</a>
    <button class="btn btn-ghost btn-sm" onclick={load} disabled={loading}>Refresh</button>
  {/snippet}
</PageToolbar>

{#if error && !notFound}
  <Notice variant="error">
    {error.message}
    {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
    <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={load}>Retry</button>
  </Notice>
{/if}

{#if loading && detail === null}
  <LoadingState label="Loading deployment" />
{:else if detail === null}
  <EmptyState
    title={notFound ? "Deployment unavailable" : "Deployment not loaded"}
    description={`No deployment matches '${deploymentId}'. It may have been removed, access may be denied, or the link may be stale.`}
    class="m-5"
  >
    {#snippet actions()}
      <a class="btn btn-outline btn-sm" href={resolve("/admin/services")}>Back to services</a>
      <button class="btn btn-ghost btn-sm" type="button" onclick={load}>Retry</button>
    {/snippet}
  </EmptyState>
{:else}
  <div
    data-testid="console-page-ready"
    class="grid gap-4 xl:grid-cols-[minmax(0,1fr)_22rem]"
  >
    <Panel title="Resource evidence">
      {#if resources.length === 0}
        {#if participantId === null}
          <EmptyState
            title="No participant installed"
            description="This deployment profile has no installed participant, so there is no grant or resource evidence. Installing a participant is a separate approval workflow."
            class="m-5"
          />
        {:else if binding === null}
          <EmptyState
            title="No grant binding"
            description="A participant is selected but no grant binding exists. This deployment is not authorized or running."
            class="m-5"
          />
        {:else}
          <EmptyState
            title="This participant has no materialized resource bindings"
            description="The installed participant declares no physical resources, or none have been reconciled yet."
            class="m-5"
          />
        {/if}
      {:else}
        <DataTable size="sm">
          <thead>
            <tr><th>Resource</th><th>Kind</th><th>State</th><th>Binding</th><th>Observed</th><th>Provider</th></tr>
          </thead>
          <tbody>
            {#each resources as resource (resource.bindingId)}
              <tr>
                <td class="trellis-identifier">{resource.localName}</td>
                <td>{resource.resourceKind}</td>
                <td>
                  <StatusBadge
                    label={resource.state}
                    status={resourceStateVariant(resource.state)}
                  />
                </td>
                <td class="trellis-identifier">{resource.bindingId}</td>
                <td>{formatTimestamp(resource.materializedAt)}</td>
                <td class="max-w-sm truncate text-xs text-base-content/70" title={providerSummary(resource.providerIdentity)}>
                  {providerSummary(resource.providerIdentity)}
                </td>
              </tr>
            {/each}
          </tbody>
        </DataTable>
        {#if resourceError}
          <Notice variant="warning" class="m-3">Resource error: {resourceError}</Notice>
        {/if}
      {/if}
    </Panel>

    <div class="space-y-4">
      <Panel title="Deployment">
        <dl class="grid grid-cols-[7rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-sm">
          <dt class="text-base-content/60">State</dt>
          <dd><StatusBadge label={detail.deployment.state} status={detail.deployment.state === "active" ? "healthy" : "offline"} /></dd>
          <dt class="text-base-content/60">Deployment</dt>
          <dd class="trellis-identifier truncate">{detail.deployment.deploymentId}</dd>
          <dt class="text-base-content/60">Participant</dt>
          <dd class="trellis-identifier truncate">{participantId ?? "Not installed"}</dd>
          <dt class="text-base-content/60">Version</dt><dd>{detail.deployment.version}</dd>
          <dt class="text-base-content/60">Updated</dt><dd>{formatTimestamp(detail.deployment.updatedAt)}</dd>
        </dl>
      </Panel>

      <Panel title="Grant binding">
        {#if binding}
          <dl class="grid grid-cols-[7rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-sm">
            <dt class="text-base-content/60">State</dt>
            <dd><StatusBadge label={binding.state} status={binding.state === "active" ? "healthy" : "offline"} /></dd>
            <dt class="text-base-content/60">Revision</dt><dd>{binding.revision}</dd>
            <dt class="text-base-content/60">Installed</dt><dd>{binding.installedRevision}</dd>
            <dt class="text-base-content/60">Permissions</dt><dd>{binding.grants.permissions.length}</dd>
            <dt class="text-base-content/60">Privileges</dt><dd>{binding.platformPrivileges.join(", ") || "None"}</dd>
          </dl>
        {:else}
          <EmptyState
            title="No grant binding"
            description="No grant binding exists for this deployment's current participant state."
          />
        {/if}
      </Panel>
    </div>
  </div>
{/if}
