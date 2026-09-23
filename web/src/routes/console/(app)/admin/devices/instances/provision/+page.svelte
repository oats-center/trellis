<script lang="ts">
  import { ulid } from "ulid";
  import { err, isErr, ok } from "@oatscenter/result";
  import { type apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import { catalogPage, traverseAll } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { captureIntent, isIntentCurrent, MutationController } from "$lib/console/mutation.ts";
  import { authorityIdentityChanged, getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import {
    TargetResolver,
    type TargetResolution,
  } from "$lib/console/target.ts";
  import { resolve } from "$lib/console_paths";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  type Deployment = apis.auth.DeploymentsListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const notifications = getNotifications();
  const RPC_TIMEOUT_MS = 10_000;

  // A deep link selects exactly this deployment; it never substitutes another.
  const requestedDeploymentId = $derived(page.url.searchParams.get("deploymentId") ?? "");
  const scope = new RequestScope("provision");
  const mutation = new MutationController<apis.auth.DevicesProvisionInput, apis.auth.DevicesProvisionOutput>();
  /** Exact target resolution for a deep-linked deployment. */
  const target = new TargetResolver<Deployment>("provision-target");

  let loading = $state(true);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let deployments = $state<Deployment[]>([]);
  /** True when the selector could not be completed; absence is not established. */
  let catalogIncomplete = $state(false);
  let provisionDeploymentId = $state("");
  /** Exact deep-link resolution; the record is the only usable target. */
  let requested = $state<TargetResolution<Deployment>>({ kind: "not-requested" });
  let instanceId = $state("");
  let identityPublicKey = $state("");
  /** Receipt for the last successful provision, with its exact identity. */
  let receipt = $state<{
    instanceId: string;
    principalId: string;
    deploymentId: string;
    secret: string;
  } | null>(null);
  const receiptTargetChanged = $derived(receipt !== null && requestedDeploymentId !== "" && receipt.deploymentId !== requestedDeploymentId);


  const activeDeployments = $derived(
    deployments.filter((deployment) =>
      deployment.state === "active" && deployment.kind === "device"
    ),
  );
  /**
   * The exact deep-linked deployment. A record of the wrong kind or state, an
   * absent record, a denied read, and a failed lookup are different states.
   */
  const requestedDeployment = $derived(
    requested.kind === "ready" ? requested.item : null,
  );
  /** The deep-link record only when it is a usable device deployment. */
  const requestedUsable = $derived(
    requestedDeployment !== null && requestedDeployment.kind === "device" &&

      requestedDeployment.state === "active",
  );
  const requestedUnavailable = $derived(
    requestedDeploymentId !== "" && requested.kind === "ready" && !requestedUsable,

  );
  const requestedMissing = $derived(
    requestedDeploymentId !== "" && requested.kind === "not-found",
  );
  const requestedForbidden = $derived(
    requestedDeploymentId !== "" && requested.kind === "forbidden",
  );
  const requestedFailed = $derived(
    requestedDeploymentId !== "" && (requested.kind === "error" || requested.kind === "incomplete"),

  );
  /**
   * The submit target: the exact deep-linked record in deep-link mode, or the
   * explicitly chosen selector value otherwise. It is asserted against the
   * resolved record immediately before dispatch.
   */
  const submitTarget = $derived(
    requestedDeploymentId !== ""
      ? requestedUsable
        ? requestedDeployment?.deploymentId ?? ""
        : ""
      : provisionDeploymentId,
  );
  const canSubmit = $derived(
    !loading && !catalogIncomplete && !pending && !uncertain && submitTarget !== "" && !requestedUnavailable &&

      !requestedMissing && !requestedForbidden && !requestedFailed,
  );

  /**
   * Resolves the deep-linked deployment through the exact `Get`, so a valid
   * deployment beyond the first list page is still selectable, and a missing,
   * disabled, or wrong-kind target is never replaced by another record.
   */
  async function loadRequested(requestedId: string): Promise<void> {
    requested = await target.resolve(requestedId, {
      // The generated Get wraps the profile; the resolver works on the profile
      // itself so both resolution paths yield the same record shape.
      get: async (id) => {
        const taken = (await trellis.deploymentsGet({ deploymentId: id }, {
          timeout: RPC_TIMEOUT_MS,
        })).take();
        return isErr(taken) ? err(taken.error) : ok(taken.deployment);
      },
      idOf: (deployment) => deployment.deploymentId,
    });
  }

  /**
   * Loads the selector catalog for the no-deep-link case. A failed later page
   * marks the catalog incomplete, which is not the same as "no deployments".
   */
  async function loadCatalog(): Promise<void> {
    const traversed = await traverseAll<Deployment>(async (pageRequest) => {
      const taken = (await trellis.deploymentsList({
        kind: "device",
        page: pageRequest,
      }, { timeout: RPC_TIMEOUT_MS })).take();
      if (isErr(taken)) throw taken.error;
      return {
        items: taken.items ?? [],
        cursor: taken.page.nextCursor ?? undefined,
      };
    });
    deployments = [...traversed.items];
    catalogIncomplete = !traversed.complete;
  }

  async function load() {
    const requestedId = requestedDeploymentId;
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      if (requestedId !== "") {
        await loadRequested(requestedId);
      } else {
        await loadCatalog();
      }
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = errorMessage(cause);
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  async function provisionInstance() {
    // The dispatch target is derived from the resolved record, never from a
    // separately drifting selection.
    const dispatchTarget = submitTarget;
    if (dispatchTarget === "" || !canSubmit) return;
    const idempotencyKey = ulid();
    const actingIdentity = authority.identity;
    const token = scope.begin();
    const intent = captureIntent<apis.auth.DevicesProvisionInput>({
      operation: "devicesProvision",
      input: {
        deploymentId: dispatchTarget,
        idempotencyKey,
        instanceId: instanceId.trim() || null,
        identityPublicKey: identityPublicKey.trim() || null,
        participantId: null,
      },
      label: dispatchTarget,
      targetId: dispatchTarget,
      scope: { routeKey: requestedDeploymentId },
      idempotencyKey,
    });
    if (!mutation.begin(intent)) return;
    pending = true;
    error = null;
    try {
      const outcome = await mutation.send({
        isStillValid: () => scope.isCurrent(token) && !disposed && isIntentCurrent(intent, { routeKey: requestedDeploymentId }) &&

          submitTarget === intent.targetId && !loading && !catalogIncomplete && !requestedUnavailable && !requestedMissing && !requestedForbidden && !requestedFailed,

        dispatch: (captured) => trellis.devicesProvision(captured.input).orThrow(),
      });
      if (!outcome) return;
      if (disposed || authorityIdentityChanged(actingIdentity, authority.identity)) { mutation.reset(); return; }
      if (outcome.kind === "unknown") {
        uncertain = true;
        error = `Provisioning outcome unknown for ${intent.label}. Inspect device instances and reload before trying again; a one-time secret cannot be recovered.`;
        return;
      }
      if (outcome.kind !== "succeeded") { error = errorMessage(outcome.error); return; }
      const response = outcome.value;
      if (response.provisioningSecret === null) {
        uncertain = true;
        error = "The instance was provisioned without a recoverable secret. Inspect device instances and reload before any further provisioning.";
        return;
      }
      // A new success replaces the previous receipt; a failed attempt never
      // re-displays the old secret as its own result.
      receipt = {
        instanceId: response.device.instanceId,
        principalId: response.device.principalId,
        deploymentId: response.device.deploymentId,
        secret: response.provisioningSecret,
      };
      notifications.success("Device instance provisioned.", "Provisioned");
      instanceId = "";
      identityPublicKey = "";
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      scope.settle(token);
      pending = false;
    }
  }

  function startAnother(): void {
    mutation.reset();
    receipt = null;
    error = null;
    instanceId = "";
    identityPublicKey = "";
  }

  // The requested ID is a live URL value, so a query-only client navigation
  // must start a new scoped read instead of reusing the mount-time target.
  $effect(() => {
    const requestedId = requestedDeploymentId;
    void load();
    return () => {
      if (requestedId !== requestedDeploymentId) {
        // The target changed; the resolver clears before the next read.
        target.clear();
      }
    };
  });

  onMount(() => {
    return () => {
      disposed = true;
      mutation.reset();
      scope.dispose();
      target.dispose();
    };
  });
</script>

<section class="space-y-4">
  <PageToolbar title="Provision device instance" description="Create a device identity and one-time provisioning secret.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant="error">{error}</Notice>
  {/if}

  {#if requestedMissing}
    <Notice variant="warning">
      No deployment matches '{requestedDeploymentId}'. Return to the device list and choose an
      existing deployment.
    </Notice>
  {:else if requestedForbidden}
    <Notice variant="warning">
      Your account is not allowed to read deployment '{requestedDeploymentId}'.
    </Notice>
  {:else if requestedUnavailable}
    <Notice variant="warning">
      Deployment '{requestedDeploymentId}' is not an active device deployment. Choose a different
      deployment or return to the device list.
    </Notice>
  {:else if requestedFailed}
    <Notice variant="error">
      Deployment '{requestedDeploymentId}' could not be resolved, so it is not known whether it
      exists. Retry before provisioning.
      <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={load}>Retry</button>
    </Notice>
  {/if}

  {#if receipt}
    {#if receiptTargetChanged}
      <Notice variant="warning">
        This one-time receipt belongs to deployment {receipt.deploymentId}, not the currently
        requested deployment {requestedDeploymentId}. Save it before provisioning another instance.
      </Notice>
    {/if}
    <Panel title="Provisioning receipt" eyebrow="One-time output">
      <dl class="grid grid-cols-[8rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-sm">
        <dt class="text-base-content/60">Deployment</dt>
        <dd class="trellis-identifier break-all">{receipt.deploymentId}</dd>
        <dt class="text-base-content/60">Instance</dt>
        <dd class="trellis-identifier break-all">{receipt.instanceId}</dd>
        <dt class="text-base-content/60">Principal</dt>
        <dd class="trellis-identifier break-all">{receipt.principalId}</dd>
        <dt class="text-base-content/60">Secret</dt>
        <dd>
          <code class="break-all select-all">{receipt.secret}</code>
        </dd>
      </dl>
      <p class="mt-3 text-xs text-base-content/60">
        Store this secret now. Trellis does not retain a readable copy; the device must
        complete activation with it.
      </p>
      <div class="mt-3 flex flex-wrap gap-2">
        <button
          class="btn btn-outline btn-sm"
          type="button"
          onclick={startAnother}
        >
          Provision another instance
        </button>
      </div>
    </Panel>
  {:else if loading}
    <Panel><LoadingState label="Loading device deployments" /></Panel>
  {:else if catalogIncomplete}
    <Panel>
      <Notice variant="error">
        The device deployment catalog could not be fully loaded, so a target cannot be declared
        absent. Retry the lookup.
        <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={load}>Retry</button>
      </Notice>
    </Panel>
  {:else if requestedDeploymentId === "" && activeDeployments.length === 0}
    <EmptyState
      title="No active device deployments"
      description="Create or enable a device deployment before provisioning instances."
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/devices/profiles/new")}>
          Create device deployment
        </a>
      {/snippet}
    </EmptyState>
  {:else}
    <Panel title="Instance identity" eyebrow="Device identity">
      <form class="trellis-form" onsubmit={(event) => { event.preventDefault(); void provisionInstance(); }}>
        <div class="trellis-record-summary">
          <div class="trellis-record-summary-title">{instanceId.trim() || "New device instance"}</div>
          <div class="trellis-metadata">Deployment {submitTarget || "not selected"}</div>
          {#if identityPublicKey.trim()}
            <div class="trellis-identifier break-all">{identityPublicKey.trim()}</div>
          {/if}
        </div>

        <div class="trellis-form-grid">
          <label class="trellis-field trellis-form-wide">
            <span class="trellis-field-label">Deployment</span>
            {#if requestedDeploymentId !== ""}
              <input
                class="input input-bordered input-sm font-mono"
                value={requestedDeployment?.deploymentId ?? requestedDeploymentId}
                readonly
                aria-label="Deployment"
              />
            {:else}
              <select
                class="select select-bordered select-sm"
                bind:value={provisionDeploymentId}
                required
              >
                <option value="" disabled>Select a deployment…</option>
                {#each activeDeployments as deployment (deployment.deploymentId)}
                  <option value={deployment.deploymentId}>{deployment.displayName} ({deployment.deploymentId})</option>
                {/each}
              </select>
            {/if}
          </label>

          <label class="trellis-field">
            <span class="trellis-field-label">Instance ID</span>
            <input class="input input-bordered input-sm font-mono" bind:value={instanceId} placeholder="Generated when omitted" />
          </label>

          <label class="trellis-field">
            <span class="trellis-field-label">Public identity key</span>
            <input class="input input-bordered input-sm font-mono" bind:value={identityPublicKey} placeholder="Generated by the device; optional" />
          </label>
        </div>

        <div class="trellis-action-row">
          <button type="submit" class="btn btn-primary btn-sm" disabled={!canSubmit}>
            {pending ? "Provisioning…" : "Provision"}
          </button>
        </div>
      </form>
    </Panel>
  {/if}
</section>
