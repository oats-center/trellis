<script lang="ts">
  import { page } from "$app/state";
  import { apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { resolve } from "$lib/console_paths";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  type Deployment = apis.auth.DeploymentsGetOutput["deployment"];
  type Instance = apis.auth.ServiceInstancesListOutput["items"][number];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("service-deployment");
  const deploymentId = $derived(page.params.deploymentId ?? "");
  let deployment = $state.raw<Deployment | null>(null);
  let instances = $state.raw<Instance[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let instanceError = $state<string | null>(null);
  let busy = $state(false);
  let uncertain = $state(false);
  let title = $state("");
  let identityPublicKey = $state("");
  let receipt = $state.raw<apis.auth.ServiceInstancesProvisionOutput | null>(
    null,
  );
  let confirmationModal: ConfirmationModal | undefined = $state();

  $effect(() => {
    const id = deploymentId;
    if (scope.setKey(id)) {
      deployment = null;
      receipt = null;
      uncertain = false;
    }
    void load(id);
  });
  async function load(id: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const input = apis.auth.API.actions["rpc:Deployments.Get"].input.decode({
        deploymentId: id,
      });
      const result = await trellis.deploymentsGet(input).orThrow();
      if (!scope.isCurrent(token)) return;
      if (result.deployment.kind !== "service")
        throw new Error("This exact deployment is not a service deployment.");
      deployment = result.deployment;
      const listed = await traverseAll(async (page) => {
        const response = await trellis.serviceInstancesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      });
      if (!scope.isCurrent(token)) return;
      instances = listed.items.filter(
        (item) => item.deploymentId === result.deployment.deploymentId,
      );
      instanceError = listed.complete
        ? null
        : projectConsoleError(listed.error).message;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }
  async function provision(): Promise<void> {
    if (
      !deployment ||
      busy ||
      uncertain ||
      loading ||
      deployment.state !== "enabled" ||
      !canPerform(authority.authority, "serviceInstancesProvision")
    )
      return;
    const target = deployment;
    const route = scope.key;
    busy = true;
    error = null;
    try {
      const input = apis.auth.API.actions[
        "rpc:ServiceInstances.Provision"
      ].input.decode({
        deploymentId: target.deploymentId,
        title: title.trim(),
        idempotencyKey: ulid(),
        ...(identityPublicKey.trim()
          ? { identityPublicKey: identityPublicKey.trim() }
          : {}),
      });
      const result = await trellis.serviceInstancesProvision(input).orThrow();
      if (scope.disposed || scope.key !== route) return;
      receipt = result;
      title = "";
      identityPublicKey = "";
      await load(target.deploymentId);
    } catch (cause) {
      if (scope.disposed || scope.key !== route) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Provisioning outcome unknown. Inspect service instances; a one-time secret cannot be recovered."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  async function disable(
    instance: Instance | undefined = undefined,
  ): Promise<void> {
    if (
      !deployment ||
      busy ||
      uncertain ||
      loading ||
      !canPerform(
        authority.authority,
        instance ? "serviceInstancesDisable" : "deploymentsDisable",
      )
    )
      return;
    const target = deployment;
    const route = scope.key;
    const key = ulid();
    const confirmed = await confirmationModal?.confirm({
      title: instance
        ? "Disable service instance?"
        : "Disable service deployment?",
      message:
        "Disable authentication eligibility for the exact target. This does not report process health.",
      confirmLabel: "Disable",
      targetLabel: instance ? "Instance" : "Deployment",
      targetName: instance?.instanceId ?? target.deploymentId,
      expectedValue: instance?.instanceId ?? target.deploymentId,
    });
    if (
      !confirmed ||
      scope.disposed ||
      scope.key !== route ||
      deployment?.revision !== target.revision
    )
      return;
    busy = true;
    try {
      if (instance)
        await trellis
          .serviceInstancesDisable({
            instanceId: instance.instanceId,
            expectedRevision: instance.revision,
            idempotencyKey: key,
          })
          .orThrow();
      else
        await trellis
          .deploymentsDisable({
            deploymentId: target.deploymentId,
            expectedRevision: target.revision,
            idempotencyKey: key,
          })
          .orThrow();
      if (!scope.disposed && scope.key === route)
        await load(target.deploymentId);
    } catch (cause) {
      if (scope.disposed || scope.key !== route) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Disable outcome unknown. Inspect the exact target before another action."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="space-y-4">
  <PageToolbar
    title={deployment?.title ?? "Service deployment"}
    description="Certified implementation binding and provisioned service identities."
    >{#snippet actions()}<a
        class="btn btn-ghost btn-sm"
        href={resolve("/admin/services")}>Back to deployments</a
      ><button
        class="btn btn-ghost btn-sm"
        disabled={loading}
        onclick={() => void load(deploymentId)}>Refresh</button
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if loading}<LoadingState
      label="Loading exact deployment"
    />{:else if deployment}
    <Panel title="Deployment" eyebrow={deployment.state}>
      <dl class="grid gap-2 text-sm sm:grid-cols-[10rem_minmax(0,1fr)]">
        <dt>Deployment</dt>
        <dd class="trellis-identifier">{deployment.deploymentId}</dd>
        <dt>Participant identity</dt>
        <dd class="trellis-identifier">{deployment.participantId}</dd>
        <dt>Implementation</dt>
        <dd class="trellis-identifier break-all">
          {deployment.implementationDigest}
        </dd>
        <dt>Revision</dt>
        <dd>{deployment.revision}</dd>
      </dl>
      {#if deployment.state === "enabled"}<button
          class="btn btn-error btn-outline btn-sm mt-3"
          disabled={busy ||
            uncertain ||
            !canPerform(authority.authority, "deploymentsDisable")}
          onclick={() => void disable()}>Disable deployment</button
        >{/if}
    </Panel>
    <Panel
      title="Service instances"
      eyebrow={`${instances.length} loaded instances`}
    >
      {#if instanceError}<Notice variant="warning"
          >Instance catalog incomplete: {instanceError}</Notice
        >{/if}
      <DataTable
        ><thead
          ><tr
            ><th>Instance / Principal</th><th>Public identity</th><th
              >Enabled</th
            ><th>Revision</th><th>Actions</th></tr
          ></thead
        ><tbody
          >{#each instances as instance (instance.instanceId)}<tr
              ><td class="trellis-identifier"
                >{instance.instanceId}
                <div class="trellis-metadata">
                  {instance.principal.principalId}
                </div></td
              ><td class="trellis-identifier"
                >{instance.identityPublicKey ?? "Not installed"}</td
              ><td>{instance.principal.enabled ? "Enabled" : "Disabled"}</td><td
                >{instance.revision}</td
              ><td
                >{#if instance.principal.enabled}<button
                    class="btn btn-ghost btn-xs"
                    disabled={busy || uncertain}
                    onclick={() => void disable(instance)}>Disable</button
                  >{/if}</td
              ></tr
            >{:else}<tr><td colspan="5">No service instances returned.</td></tr
            >{/each}</tbody
        ></DataTable
      >
    </Panel>
    {#if receipt}<Panel title="Provisioning receipt"
        ><p class="trellis-identifier">
          Instance {receipt.instance.instanceId} · Principal {receipt.instance
            .principal.principalId}
        </p>
        {#if receipt.provisioningSecret}<code
            class="block break-all select-all mt-3"
            >{receipt.provisioningSecret}</code
          >
          <p class="trellis-field-help">
            Store this one-time secret now. Trellis does not retain a readable
            copy.
          </p>{:else}<p>
            Public identity installed; no provisioning secret issued.
          </p>{/if}</Panel
      >{/if}
    {#if deployment.state === "enabled"}<Panel
        title="Provision service instance"
        ><form
          class="trellis-form"
          onsubmit={(event) => {
            event.preventDefault();
            void provision();
          }}
        >
          <label class="trellis-field"
            ><span class="trellis-field-label">Instance title</span><input
              class="input input-bordered input-sm"
              bind:value={title}
              required
            /></label
          ><label class="trellis-field"
            ><span class="trellis-field-label"
              >Public identity key (optional)</span
            ><input
              class="input input-bordered input-sm font-mono"
              bind:value={identityPublicKey}
            /><span class="trellis-field-help"
              >Generated by the service. Private keys are never stored by
              Trellis.</span
            ></label
          >
          <div class="trellis-action-row">
            <button
              class="btn btn-primary btn-sm"
              type="submit"
              disabled={busy ||
                uncertain ||
                !title.trim() ||
                !canPerform(authority.authority, "serviceInstancesProvision")}
              >Provision</button
            >
          </div>
        </form></Panel
      >{/if}
  {/if}
</section>
<ConfirmationModal bind:this={confirmationModal} />
