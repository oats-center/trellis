<script lang="ts">
  import { ulid } from "ulid";
  import { type apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { authorityIdentityChanged, getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, isIntentCurrent, MutationController } from "$lib/console/mutation.ts";
  import { resolve } from "$lib/console_paths";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage } from "$lib/format";
  import { getNotifications } from "$lib/notifications.svelte";
  import { getTrellis } from "$lib/trellis";

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const mutation = new MutationController<apis.auth.DeploymentsCreateInput, apis.auth.DeploymentsCreateOutput>();
  const notifications = getNotifications();

  let error = $state<string | null>(null);
  let pending = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let displayName = $state("");
  let reviewMode = $state<"none" | "required">("none");
  let requiresDeviceDelegation = $state(false);
  /** Set when the create succeeded so the operator keeps the allocated ID. */
  let createdDeploymentId = $state<string | null>(null);

  async function createDeployment() {
    if (pending || uncertain) return;
    const idempotencyKey = ulid();
    const actingIdentity = authority.identity;
    const capturedReviewMode = reviewMode;
    const input: apis.auth.DeploymentsCreateInput = {
      displayName: displayName.trim(),
      expiresAt: null,
      idempotencyKey,
      kind: "device",
      participantId: null,
      portalId: null,
      requiresDeviceDelegation,
      reviewMode: new TextEncoder().encode(JSON.stringify(reviewMode)),
    };
    if (!input.displayName) return;
    const intent = captureIntent({
      operation: "deploymentsCreate",
      input,
      label: input.displayName,
      targetId: "new-device-deployment",
      scope: { routeKey: "new-device-deployment" },
      idempotencyKey,
    });
    if (!mutation.begin(intent)) return;
    pending = true;
    error = null;
    try {
      const outcome = await mutation.send({
        isStillValid: () => !disposed && isIntentCurrent(intent, { routeKey: "new-device-deployment" }) &&

          displayName.trim() === intent.input.displayName && requiresDeviceDelegation === intent.input.requiresDeviceDelegation &&

          reviewMode === capturedReviewMode,
        dispatch: (captured) => trellis.deploymentsCreate(captured.input).orThrow(),
      });
      if (!outcome) return;
      if (disposed || authorityIdentityChanged(actingIdentity, authority.identity)) { mutation.reset(); return; }
      if (outcome.kind === "unknown") {
        uncertain = true;
        error = `Creation outcome unknown for ${intent.label}. Inspect device deployments and reload before attempting another create.`;
        return;
      }
      if (outcome.kind !== "succeeded") { error = errorMessage(outcome.error); return; }
      const response = outcome.value;
      createdDeploymentId = response.deployment.deploymentId;
      notifications.success(`Device deployment ${createdDeploymentId} created.`, "Created");
      displayName = "";
      reviewMode = "none";
      requiresDeviceDelegation = false;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      pending = false;
    }
  }

  onMount(() => () => { disposed = true; mutation.reset(); });
</script>

<section class="space-y-4">
  <PageToolbar title="Create device deployment" description="Create a deployment that controls device activation review requirements.">
    {#snippet actions()}
      <a class="btn btn-ghost btn-sm" href={resolve("/admin/devices")}>Back to devices</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant={uncertain ? "warning" : "error"}>{error}</Notice>
  {/if}

  {#if createdDeploymentId}
    <Notice variant="success">
      Last confirmed device deployment profile: {createdDeploymentId}. Provisioning a device instance is a separate step.
      <a class="link ml-1" href={resolve("/admin/devices/instances/provision")} data-provision-link>Provision device</a>
    </Notice>
  {/if}

  <Panel title="Deployment details" eyebrow="Device authorization">
    <form class="grid gap-3 lg:grid-cols-2" onsubmit={(event) => { event.preventDefault(); void createDeployment(); }}>
      <label class="form-control gap-1">
        <span class="label-text text-xs">Display name</span>
        <input class="input input-bordered input-sm" bind:value={displayName} placeholder="Reader fleet" required />
        <span class="label-text-alt text-base-content/60">
          Labels the deployment profile. The deployment ID is allocated by the server.
        </span>
      </label>

      <label class="form-control gap-1">
        <span class="label-text text-xs">Review mode</span>
        <select class="select select-bordered select-sm" bind:value={reviewMode}>
          <option value="none">No review</option>
          <option value="required">Review required</option>
        </select>
      </label>

      <label class="form-control gap-1 lg:col-span-2">
        <span class="label cursor-pointer justify-start gap-3 rounded-box border border-base-300 px-3 py-2">
          <input class="checkbox checkbox-sm" type="checkbox" bind:checked={requiresDeviceDelegation} />
          <span>
            <span class="label-text text-sm">Require user delegation</span>
            <span class="block text-xs text-base-content/60">Require an activating user to grant device authority. Administrative review remains controlled separately above.</span>
          </span>
        </span>
      </label>

      <div class="flex items-end justify-end lg:col-span-2">
        <button type="submit" class="btn btn-outline btn-sm" disabled={pending || uncertain || displayName.trim().length === 0}>
          {pending ? "Creating…" : "Create deployment profile"}
        </button>
      </div>
    </form>
  </Panel>
</section>
