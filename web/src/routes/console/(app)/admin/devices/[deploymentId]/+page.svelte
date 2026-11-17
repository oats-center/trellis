<script lang="ts">
  import { humanizeName } from "$lib/format";
  import { page } from "$app/state";
  import { apis } from "$lib/trellis-apis";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { resolve } from "$lib/console_paths";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import ParticipantSettings from "$lib/components/ParticipantSettings.svelte";

  type Deployment = apis.auth.DeploymentsGetOutput["deployment"];
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("device-type");
  const deploymentId = $derived(page.params.deploymentId ?? "");
  let deployment = $state.raw<Deployment | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);

  $effect(() => {
    const id = deploymentId;
    if (scope.setKey(id)) deployment = null;
    void load(id);
  });

  async function load(id: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    try {
      const result = await trellis
        .deploymentsGet(
          apis.auth.API.actions["rpc:Deployments.Get"].input.decode({
            deploymentId: id,
          }),
        )
        .orThrow();
      if (!scope.isCurrent(token)) return;
      if (result.deployment.kind !== "device")
        throw new Error("This is not a device type.");
      deployment = result.deployment;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }
  onMount(() => () => scope.dispose());
</script>

<section class="max-w-5xl space-y-5">
  <PageToolbar
    eyebrow="Device types"
    title={deployment ? humanizeName(deployment.title) : "Device type"}
    description={deployment
      ? humanizeName(deployment.participantId)
      : undefined}
    >{#snippet actions()}<a
        class="btn btn-ghost"
        href={resolve("/admin/devices")}>Back to devices</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if loading}<LoadingState
      label="Loading device type"
    />{:else if deployment && canPerform(authority.authority, "configGet")}
    <ParticipantSettings deploymentId={deployment.deploymentId} />
  {/if}
</section>
