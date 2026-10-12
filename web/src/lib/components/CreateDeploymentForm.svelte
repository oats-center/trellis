<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import Panel from "$lib/components/Panel.svelte";

  let { kind }: { kind: "service" | "device" } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  let title = $state("");
  let participantId = $state("");
  let evidence = $state("");
  let busy = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let error = $state<string | null>(null);
  let created = $state<apis.auth.DeploymentsCreateOutput | null>(null);
  const back = $derived(
    kind === "service" ? "/admin/services" : "/admin/devices",
  );
  async function create(): Promise<void> {
    if (
      busy ||
      uncertain ||
      created ||
      !canPerform(authority.authority, "deploymentsCreate")
    )
      return;
    busy = true;
    error = null;
    try {
      const input = apis.auth.API.actions[
        "rpc:Deployments.Create"
      ].input.decode({
        title: title.trim(),
        kind,
        participantId: participantId.trim(),
        packageEvidence: JSON.parse(evidence),
        idempotencyKey: ulid(),
      });
      const response = await trellis.deploymentsCreate(input).orThrow();
      if (!disposed) created = response;
    } catch (cause) {
      if (disposed) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      error = uncertain
        ? "Creation outcome unknown. Inspect the deployment catalog before creating again."
        : projectConsoleError(cause).message;
    } finally {
      if (!disposed) busy = false;
    }
  }
  onMount(() => () => {
    disposed = true;
  });
</script>

<section class="space-y-4">
  <PageToolbar
    title={`Create ${kind} deployment`}
    description="Compile native package evidence into an implementation and bind a new deployment to it."
    >{#snippet actions()}<a class="btn btn-ghost btn-sm" href={resolve(back)}
        >Back to deployments</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if created}
    <Notice variant="success"
      >Deployment {created.deployment.deploymentId} created. Implementation {created
        .deployment.implementationDigest}. Revision {created.deployment
        .revision}.</Notice
    >
    <a
      class="btn btn-outline btn-sm"
      href={resolve(
        kind === "service"
          ? `/admin/services/${created.deployment.deploymentId}`
          : "/admin/devices",
      )}>Open deployment</a
    >
  {:else}
    <Panel
      title="Native deployment definition"
      eyebrow={`${kind} deployment`}
      class="max-w-3xl"
    >
      <form
        class="trellis-form"
        onsubmit={(event) => {
          event.preventDefault();
          void create();
        }}
      >
        <label class="trellis-field"
          ><span class="trellis-field-label">Title</span><input
            class="input input-bordered input-sm"
            bind:value={title}
            required
            disabled={busy || uncertain}
          /></label
        >
        <label class="trellis-field"
          ><span class="trellis-field-label">Participant identity</span><input
            class="input input-bordered input-sm font-mono"
            bind:value={participantId}
            required
            disabled={busy || uncertain}
          /><span class="trellis-field-help"
            >Exact participant identity authored in the native package. This is
            not a retired participant registration or a permission grant.</span
          ></label
        >
        <label class="trellis-field"
          ><span class="trellis-field-label"
            >Native package source evidence</span
          ><textarea
            class="textarea textarea-bordered min-h-48 font-mono"
            bind:value={evidence}
            required
            disabled={busy || uncertain}
            aria-describedby="package-evidence-help"
          ></textarea><span
            id="package-evidence-help"
            class="trellis-field-help"
            >JSON packageName, packageVersion, sources (alias, path, source),
            and dependencies. Source entries contain native Trellis IDL, not
            canonical contract JSON. The generated input codec validates the
            request; Auth compiles and verifies the implementation.</span
          ></label
        >
        <div class="trellis-action-row">
          <button
            type="submit"
            class="btn btn-primary btn-sm"
            disabled={busy ||
              uncertain ||
              !title.trim() ||
              !participantId.trim() ||
              !evidence.trim()}
            >{busy ? "Creating…" : "Create deployment"}</button
          >
        </div>
      </form>
    </Panel>
  {/if}
</section>
