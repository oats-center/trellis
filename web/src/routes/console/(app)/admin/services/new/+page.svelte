<script lang="ts">
  import { ulid } from "ulid";
  import { type apis } from "trellis-web-generated";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
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
  let createPending = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let displayName = $state("");
  /** Set when the create succeeded but navigation did not, so the operator
   * keeps a receipt for the committed deployment instead of a blank form. */
  let createdDeploymentId = $state<string | null>(null);

  async function createDeployment() {
    if (createPending || uncertain || createdDeploymentId !== null) return;
    const idempotencyKey = ulid();
    const actingIdentity = authority.identity;
    const input: apis.auth.DeploymentsCreateInput = {
      displayName: displayName.trim(),
      expiresAt: null,
      idempotencyKey,
      kind: "service",
      participantId: null,
      portalId: null,
      requiresDeviceDelegation: false,
      reviewMode: null,
    };
    if (!input.displayName) return;
    const intent = captureIntent({
      operation: "deploymentsCreate",
      input,
      label: input.displayName,
      targetId: "new-service-deployment",
      scope: { routeKey: "new-service-deployment" },
      idempotencyKey,
    });
    if (!mutation.begin(intent)) return;
    createPending = true;
    error = null;
    try {
      const outcome = await mutation.send({
        isStillValid: () => !disposed && isIntentCurrent(intent, { routeKey: "new-service-deployment" }) &&

          displayName.trim() === intent.input.displayName,
        dispatch: (captured) => trellis.deploymentsCreate(captured.input).orThrow(),
      });
      if (!outcome) return;
      if (disposed || authorityIdentityChanged(actingIdentity, authority.identity)) { mutation.reset(); return; }
      if (outcome.kind === "unknown") {
        uncertain = true;
        error = `Creation outcome unknown for ${intent.label}. Inspect service deployments and reload before attempting another create.`;
        return;
      }
      if (outcome.kind !== "succeeded") { error = errorMessage(outcome.error); return; }
      const created = outcome.value;
      const deploymentId = created.deployment.deploymentId;
      createdDeploymentId = deploymentId;
      notifications.success(`Service deployment profile ${intent.label} created.`, "Created");
      displayName = "";
      const target = resolve("/admin/services/[deploymentId]", { deploymentId });
      try {
        await goto(target);
        createdDeploymentId = null;
      } catch (navigationError) {
        error = `Deployment ${deploymentId} was created, but navigation failed: ${
          errorMessage(navigationError)
        }`;
      }
    } catch (cause) {
      error = errorMessage(cause);
    } finally {
      createPending = false;
    }
  }

  onMount(() => () => { disposed = true; mutation.reset(); });
</script>

<section class="space-y-4">
  <PageToolbar title="Create service deployment" description="Create a service deployment profile.">
    {#snippet actions()}
      <a href={resolve("/admin/services")} class="btn btn-ghost btn-sm">Back to service deployments</a>
    {/snippet}
  </PageToolbar>

  {#if error}
    <Notice variant={uncertain ? "warning" : "error"}>{error}</Notice>
  {/if}

  {#if createdDeploymentId}
    <Notice variant="success">
      Deployment profile {createdDeploymentId} was created but the console could not open it.
      <a
        class="link ml-1"
        href={resolve("/admin/services/[deploymentId]", { deploymentId: createdDeploymentId })}
      >Open deployment</a>
    </Notice>
  {/if}

  <Panel title="New deployment profile" eyebrow="Service deployment" class="max-w-3xl">
    <form class="grid gap-4" onsubmit={(event) => { event.preventDefault(); void createDeployment(); }}>
      <label class="form-control gap-1">
        <span class="label-text text-xs">Display name</span>
        <input
          class="input input-bordered input-sm"
          bind:value={displayName}
          placeholder="Billing worker"
          required
        />
        <span class="label-text-alt text-base-content/60">
          Labels the deployment profile. The deployment ID is allocated by the server.
        </span>
      </label>

      <p class="rounded-box border border-base-300 bg-base-200/50 p-3 text-xs text-base-content/70">
        This creates a deployment profile only. It does not install a participant,
        start a service, or grant authority; installing a participant is a separate
        approval workflow.
      </p>

      <div class="flex flex-wrap justify-end gap-2">
        <a href={resolve("/admin/services")} class="btn btn-ghost btn-sm">Cancel</a>
        <button
          type="submit"
          class="btn btn-outline btn-sm"
          disabled={createPending || uncertain || createdDeploymentId !== null || displayName.trim().length === 0}
        >
          {createPending ? "Creating…" : "Create deployment profile"}
        </button>
      </div>
    </form>
  </Panel>
</section>
