<script lang="ts">
  import { type apis } from "$lib/trellis-apis";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import PublishedPackagePicker from "$lib/components/PublishedPackagePicker.svelte";

  type Deployment = apis.auth.DeploymentsGetOutput["deployment"];

  let {
    deployment,
    onapplied,
  }: {
    deployment: Deployment;
    /** Called with the deployment once its new build is installed. */
    onapplied: (deployment: Deployment) => void;
  } = $props();

  // Installing a build ahead of rolling it out lets instances that run it
  // connect at once, with nothing left to approve.
  const trellis = getTrellis();
  let open = $state(false);
  let pulled = $state.raw<apis.auth.PackagesPullOutput | null>(null);
  let busy = $state(false);
  let uncertain = $state(false);
  let error = $state<string | null>(null);
  let sharing = $state<string | null>(null);
  let sharingUnderstood = $state(false);
  let installed = $state<string | null>(null);
  let disposed = false;

  const provides = $derived(
    pulled?.participants.some(
      (participant) => participant.participantId === deployment.participantId,
    ) ?? false,
  );
  const ready = $derived(
    pulled !== null &&
      provides &&
      !busy &&
      !uncertain &&
      (sharing === null || sharingUnderstood),
  );

  function choose(next: apis.auth.PackagesPullOutput | null): void {
    pulled = next;
    error = null;
    sharing = null;
    installed = null;
  }

  async function install(): Promise<void> {
    if (!ready || !pulled) return;
    const release = pulled;
    busy = true;
    error = null;
    try {
      const response = await trellis
        .deploymentsApply({
          deploymentId: deployment.deploymentId,
          expectedRevision: deployment.revision,
          packageEvidence: release.packageEvidence,
          ...(sharing && sharingUnderstood
            ? { acknowledgeSharedApis: true }
            : {}),
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (disposed) return;
      installed = `${release.published.package} ${release.published.version}`;
      pulled = null;
      open = false;
      onapplied(response.deployment);
    } catch (cause) {
      if (disposed) return;
      uncertain = classifyMutationError(cause).kind === "unknown";
      const message = projectConsoleError(cause).message;
      if (!uncertain && message.includes("acknowledgeSharedApis")) {
        sharing = message;
        sharingUnderstood = false;
        return;
      }
      error = uncertain
        ? "Couldn't tell whether the build was installed. Refresh before trying again."
        : message;
    } finally {
      if (!disposed) busy = false;
    }
  }

  onMount(() => () => {
    disposed = true;
  });
</script>

<section class="trellis-section" aria-labelledby="install-build-title">
  <div class="flex flex-wrap items-center justify-between gap-3 px-5 py-4">
    <div>
      <h2 id="install-build-title" class="text-base font-semibold">
        Install a build
      </h2>
      {#if installed}<p class="text-sm text-success" role="status">
          Installed {installed}. Instances running it connect at once.
        </p>{/if}
    </div>
    <button
      class="btn btn-outline btn-sm"
      aria-expanded={open}
      onclick={() => {
        open = !open;
        pulled = null;
        installed = null;
      }}>From a published package</button
    >
  </div>
  {#if open}
    <form
      class="trellis-form border-t border-base-300 bg-base-200/40 px-5 py-4"
      aria-label="Install a published build"
      onsubmit={(event) => {
        event.preventDefault();
        void install();
      }}
    >
      <PublishedPackagePicker disabled={busy || uncertain} onpulled={choose} />
      {#if pulled && !provides}<p class="text-sm text-error">
          This package doesn't provide {deployment.participantId}.
        </p>{/if}
      {#if sharing}
        <div class="alert alert-warning block space-y-2" role="alert">
          <p class="font-semibold">
            Another service already answers an API this build provides.
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
      {#if error}<p class="text-sm text-error" role="alert">{error}</p>{/if}
      <div class="trellis-action-row">
        <button
          class="btn btn-ghost"
          type="button"
          onclick={() => (open = false)}>Cancel</button
        >
        <button class="btn btn-primary" type="submit" disabled={!ready}
          >{busy ? "Installing…" : "Install build"}</button
        >
      </div>
    </form>
  {/if}
</section>
