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

<section class="max-w-3xl space-y-5">
  <PageToolbar
    eyebrow={kind === "service" ? "Services" : "Devices"}
    title={kind === "service" ? "Add a service" : "New device type"}
    description={kind === "service"
      ? "Services are usually added with the trellis command-line tool. You can also paste a package here."
      : "A device type describes software that many devices run."}
    >{#snippet actions()}<a class="btn btn-ghost" href={resolve(back)}
        >Back to {kind === "service" ? "services" : "devices"}</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if created}
    <div class="trellis-section p-5">
      <p class="font-medium">{created.deployment.title} was added.</p>
      <details class="mt-2 text-sm">
        <summary class="cursor-pointer text-base-content/70"
          >Technical details</summary
        >
        <p class="mt-2 text-xs">
          ID <span class="trellis-identifier" data-testid="created-id"
            >{created.deployment.deploymentId}</span
          >
        </p>
      </details>
      <a
        class="btn btn-primary mt-4"
        href={resolve(
          kind === "service"
            ? `/admin/services/${created.deployment.deploymentId}`
            : "/admin/devices",
        )}>{kind === "service" ? "Open service" : "Go to devices"}</a
      >
    </div>
  {:else}
    <form
      class="trellis-section trellis-form p-5"
      onsubmit={(event) => {
        event.preventDefault();
        void create();
      }}
    >
      <label class="trellis-field"
        ><span class="trellis-field-label">Name</span><input
          class="input w-full"
          bind:value={title}
          required
          disabled={busy || uncertain}
        /></label
      >
      <div class="trellis-field">
        <label class="trellis-field-label" for="deployment-participant"
          >Participant name</label
        ><input
          id="deployment-participant"
          class="input w-full font-mono"
          bind:value={participantId}
          placeholder="acme.orders"
          aria-describedby="deployment-participant-help"
          required
          disabled={busy || uncertain}
        /><span id="deployment-participant-help" class="trellis-field-help"
          >The {kind} name declared in the package.</span
        >
      </div>
      <div class="trellis-field">
        <label class="trellis-field-label" for="deployment-package"
          >Package</label
        ><textarea
          id="deployment-package"
          class="textarea min-h-48 w-full font-mono text-xs"
          bind:value={evidence}
          required
          disabled={busy || uncertain}
          aria-describedby="package-evidence-help"
        ></textarea><span id="package-evidence-help" class="trellis-field-help"
          >The package JSON with its sources and dependencies.</span
        >
      </div>
      <div class="trellis-action-row">
        <button
          type="submit"
          class="btn btn-primary"
          disabled={busy ||
            uncertain ||
            !title.trim() ||
            !participantId.trim() ||
            !evidence.trim()}
          >{busy
            ? "Adding…"
            : kind === "service"
              ? "Add service"
              : "Add device type"}</button
        >
      </div>
    </form>
  {/if}
</section>
