<script lang="ts">
  import { formatDate, humanizeName } from "$lib/format";
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
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import ServiceUpdates from "$lib/components/ServiceUpdates.svelte";

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
        ? "We couldn't confirm the change. Refresh before trying again."
        : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="max-w-5xl space-y-5">
  <PageToolbar
    eyebrow="Services"
    title={deployment ? humanizeName(deployment.title) : "Service"}
    description={deployment
      ? humanizeName(deployment.participantId)
      : undefined}
    >{#snippet actions()}<a
        class="btn btn-ghost"
        href={resolve("/admin/services")}>Back to services</a
      ><button
        class="btn btn-ghost"
        disabled={loading}
        onclick={() => void load(deploymentId)}>Refresh</button
      >{#if deployment?.state === "enabled"}<button
          class="btn btn-outline btn-error"
          disabled={busy ||
            uncertain ||
            !canPerform(authority.authority, "deploymentsDisable")}
          onclick={() => void disable()}>Disable service</button
        >{/if}{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if deployment && canPerform(authority.authority, "serviceUpdatesList")}<ServiceUpdates
      deploymentId={deployment.deploymentId}
      focus={page.url.searchParams.get("update") ?? undefined}
      onapplied={() => void load(deploymentId)}
    />{/if}
  {#if loading}<LoadingState label="Loading service" />{:else if deployment}
    <div class="trellis-section p-5">
      <div class="flex flex-wrap items-center gap-3">
        <span
          class={[
            "badge",
            deployment.state === "enabled" ? "badge-success" : "badge-neutral",
          ]}>{deployment.state === "enabled" ? "Active" : "Disabled"}</span
        >
        <span class="text-sm text-base-content/65"
          >{instances.filter((item) => item.principal.enabled).length} of {instances.length}
          instances turned on</span
        >
      </div>
      <details class="mt-4 text-sm">
        <summary class="cursor-pointer text-base-content/70"
          >Technical details</summary
        >
        <dl
          class="mt-3 grid gap-x-4 gap-y-1.5 text-xs sm:grid-cols-[9rem_minmax(0,1fr)]"
        >
          <dt class="text-base-content/60">Service ID</dt>
          <dd class="trellis-identifier">{deployment.deploymentId}</dd>
          <dt class="text-base-content/60">Contract</dt>
          <dd class="trellis-identifier">{deployment.participantId}</dd>
          <dt class="text-base-content/60">Build</dt>
          <dd class="trellis-identifier break-all">
            {deployment.implementationDigest}
          </dd>
          <dt class="text-base-content/60">Revision</dt>
          <dd>{deployment.revision}</dd>
        </dl>
      </details>
    </div>

    <div class="trellis-section">
      <h2 class="px-5 pt-5 text-base font-semibold">Instances</h2>
      <p class="px-5 text-sm text-base-content/65">
        Each running copy of this service.
      </p>
      {#if instanceError}<Notice variant="warning" class="mx-5 mt-3"
          >Some instances couldn't be loaded: {instanceError}</Notice
        >{/if}
      <ul class="mt-3 divide-y divide-base-300 border-t border-base-300">
        {#each instances as instance (instance.instanceId)}
          <li class="flex items-center justify-between gap-3 px-5 py-3">
            <span class="min-w-0">
              <span class="block font-medium"
                >{instance.principal.title &&
                instance.principal.title !== instance.instanceId
                  ? instance.principal.title
                  : `Instance ${instance.instanceId.slice(-6)}`}</span
              >
              <span class="block text-xs text-base-content/60"
                >Added {formatDate(instance.principal.createdAt)}</span
              >
            </span>
            <span class="flex items-center gap-3">
              <span
                class={[
                  "badge badge-sm",
                  instance.principal.enabled
                    ? "badge-success"
                    : "badge-neutral",
                ]}>{instance.principal.enabled ? "On" : "Off"}</span
              >
              {#if instance.principal.enabled}<button
                  class="btn btn-ghost btn-xs text-error"
                  disabled={busy || uncertain}
                  onclick={() => void disable(instance)}>Turn off</button
                >{/if}
            </span>
          </li>
        {:else}
          <li class="px-5 py-4 text-sm text-base-content/60">
            No instances yet.
          </li>
        {/each}
      </ul>
    </div>

    {#if receipt}<Panel title="Provisioning receipt"
        ><p class="text-sm">
          Added {receipt.instance.principal.title || "a new instance"}.
        </p>
        {#if receipt.provisioningSecret}<p class="trellis-field-help mt-2">
            Give this one-time setup key to the new instance. You won't see it
            again.
          </p>
          <code
            class="mt-2 block break-all rounded-field bg-base-200 p-3 select-all"
            >{receipt.provisioningSecret}</code
          >{:else}<p class="trellis-field-help mt-2">
            It will connect with its own key; no setup key is needed.
          </p>{/if}</Panel
      >{/if}
    {#if deployment.state === "enabled"}<Panel title="Add an instance"
        ><form
          class="trellis-form"
          onsubmit={(event) => {
            event.preventDefault();
            void provision();
          }}
        >
          <label class="trellis-field"
            ><span class="trellis-field-label">Instance title</span><input
              class="input w-full"
              bind:value={title}
              required
            /></label
          >
          <details class="text-sm">
            <summary class="cursor-pointer text-base-content/70"
              >Use the instance's own key</summary
            >
            <label class="trellis-field mt-2"
              ><span class="trellis-field-label">Public key</span><input
                class="input w-full font-mono"
                bind:value={identityPublicKey}
              /><span class="trellis-field-help"
                >Leave empty to get a one-time setup key instead.</span
              ></label
            >
          </details>
          <div class="trellis-action-row">
            <button
              class="btn btn-primary"
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
