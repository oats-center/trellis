<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { ulid } from "ulid";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
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
  // A service can also be added without its package: whatever first connects
  // with the given identity asks for what it needs, and approving it adds it.
  let mode = $state<"package" | "claim">("package");
  let identityPublicKey = $state("");
  let participantId = $state("");
  let evidence = $state("");
  let busy = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let error = $state<string | null>(null);
  // Auth refused because another service already answers an API this package
  // implements; creating it anyway needs the administrator's explicit consent.
  let sharing = $state<string | null>(null);
  let sharingUnderstood = $state(false);
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
        ...(sharing && sharingUnderstood
          ? { acknowledgeSharedApis: true }
          : {}),
        idempotencyKey: ulid(),
      });
      const response = await trellis.deploymentsCreate(input).orThrow();
      if (!disposed) created = response;
    } catch (cause) {
      if (disposed) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      const message = projectConsoleError(cause).message;
      if (!uncertain && message.includes("acknowledgeSharedApis")) {
        sharing = message;
        sharingUnderstood = false;
        error = null;
        return;
      }
      error = uncertain
        ? "Creation outcome unknown. Inspect the deployment catalog before creating again."
        : message;
    } finally {
      if (!disposed) busy = false;
    }
  }
  async function createClaim(): Promise<void> {
    if (busy || !canPerform(authority.authority, "serviceClaimsCreate")) return;
    busy = true;
    error = null;
    try {
      const response = await trellis
        .serviceClaimsCreate({
          title: title.trim(),
          instanceTitle: title.trim(),
          identityPublicKey: identityPublicKey.trim(),
          idempotencyKey: ulid(),
        })
        .orThrow();
      if (!disposed)
        await goto(
          resolve("/admin/services/[deploymentId]", {
            deploymentId: response.claim.claimId,
          }),
        );
    } catch (cause) {
      if (!disposed) error = projectConsoleError(cause).message;
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
    {#if kind === "service"}
      <div class="trellis-segment" role="group" aria-label="How to add it">
        <button
          type="button"
          class={{ active: mode === "package" }}
          aria-pressed={mode === "package"}
          onclick={() => (mode = "package")}>I have its package</button
        >
        <button
          type="button"
          class={{ active: mode === "claim" }}
          aria-pressed={mode === "claim"}
          onclick={() => (mode = "claim")}>It will introduce itself</button
        >
      </div>
    {/if}
    {#if mode === "claim"}
      <form
        class="trellis-section trellis-form p-5"
        onsubmit={(event) => {
          event.preventDefault();
          void createClaim();
        }}
      >
        <label class="trellis-field"
          ><span class="trellis-field-label">Name</span><input
            class="input w-full"
            bind:value={title}
            required
            disabled={busy}
          /></label
        >
        <div class="trellis-field">
          <label class="trellis-field-label" for="claim-public-key"
            >Service's public key</label
          ><input
            id="claim-public-key"
            class="input w-full font-mono"
            bind:value={identityPublicKey}
            aria-describedby="claim-public-key-help"
            required
            disabled={busy}
          /><span id="claim-public-key-help" class="trellis-field-help"
            >Its owner keeps the matching private seed.</span
          >
        </div>
        <div class="trellis-action-row">
          <button
            type="submit"
            class="btn btn-primary"
            disabled={busy || !title.trim() || !identityPublicKey.trim()}
            >{busy ? "Adding…" : "Add service"}</button
          >
        </div>
      </form>
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
          ></textarea><span
            id="package-evidence-help"
            class="trellis-field-help"
            >The package JSON with its sources and dependencies.</span
          >
        </div>
        {#if sharing}
          <div class="alert alert-warning block space-y-2" role="alert">
            <p class="font-semibold">
              Another service already answers an API this package provides.
            </p>
            <p class="text-sm">
              Adding it makes both services answer the same requests. Each
              request goes to one of them at random, so each sees only some
              requests, they keep separate data, and callers can't tell which
              one answered. To add capacity or move a service, add instances to
              the existing service instead.
            </p>
            <details class="text-xs">
              <summary class="cursor-pointer">Details</summary>
              <p class="mt-1">{sharing}</p>
            </details>
            <label class="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                class="checkbox checkbox-sm"
                bind:checked={sharingUnderstood}
              />
              I understand requests will be split between the two services
            </label>
          </div>
        {/if}
        <div class="trellis-action-row">
          <button
            type="submit"
            class="btn btn-primary"
            disabled={busy ||
              uncertain ||
              (sharing !== null && !sharingUnderstood) ||
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
  {/if}
</section>
