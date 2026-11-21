<script lang="ts">
  import { type apis } from "$lib/trellis-apis";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { projectConsoleError } from "$lib/console/display_value.ts";

  type Pulled = apis.auth.PackagesPullOutput;

  let {
    disabled = false,
    onpulled,
  }: {
    disabled?: boolean;
    /** The release Trellis pulled and checked, or null once it no longer applies. */
    onpulled: (pulled: Pulled | null) => void;
  } = $props();

  const trellis = getTrellis();
  let name = $state("");
  let release = $state("");
  let pulling = $state(false);
  let error = $state<string | null>(null);
  let pulled = $state.raw<Pulled | null>(null);
  // A later lookup wins over one still running.
  let lookups = 0;
  let disposed = false;

  const ready = $derived(
    !disabled && !pulling && name.trim() !== "" && release.trim() !== "",
  );

  onMount(() => () => {
    disposed = true;
  });

  /** Forget a looked-up release once what it was looked up by changes. */
  function changed(): void {
    lookups++;
    pulling = false;
    if (pulled) {
      pulled = null;
      onpulled(null);
    }
  }

  async function lookUp(): Promise<void> {
    if (!ready) return;
    const lookup = ++lookups;
    const wanted = release.trim();
    pulling = true;
    error = null;
    // The release looked up before no longer stands for what is asked now,
    // even if the lookup fails or never returns.
    if (pulled) {
      pulled = null;
      onpulled(null);
    }
    try {
      const response = await trellis
        .packagesPull({
          package: name.trim(),
          ...(wanted.startsWith("sha256:")
            ? { digest: wanted }
            : { version: wanted }),
        })
        .orThrow();
      if (disposed || lookup !== lookups) return;
      pulled = response;
      onpulled(response);
    } catch (cause) {
      if (disposed || lookup !== lookups) return;
      error = projectConsoleError(cause).message;
    } finally {
      if (!disposed && lookup === lookups) pulling = false;
    }
  }
</script>

<div class="space-y-3">
  <div class="grid gap-3 sm:grid-cols-3">
    <label class="trellis-field sm:col-span-2"
      ><span class="trellis-field-label">Package</span><input
        class="input w-full font-mono"
        bind:value={name}
        autocomplete="off"
        spellcheck="false"
        placeholder="ghcr.io/acme/orders"
        {disabled}
        oninput={changed}
      /></label
    >
    <label class="trellis-field"
      ><span class="trellis-field-label">Version</span><input
        class="input w-full font-mono"
        bind:value={release}
        autocomplete="off"
        spellcheck="false"
        placeholder="1.0.0"
        {disabled}
        oninput={changed}
      /></label
    >
  </div>
  <button
    type="button"
    class="btn btn-outline btn-sm"
    disabled={!ready}
    onclick={() => void lookUp()}
    >{pulling ? "Looking it up…" : "Look up package"}</button
  >
  {#if error}<p class="text-sm text-error" role="alert">{error}</p>{/if}
  {#if pulled}
    <div
      class="rounded-box border border-base-300 p-3 text-sm"
      role="region"
      aria-label="Published package"
    >
      <p class="font-medium">
        {pulled.published.package}
        {pulled.published.version}
      </p>
      {#if pulled.dependencies.length > 0}
        <p class="text-base-content/70">
          Built with {pulled.dependencies
            .map((dependency) => `${dependency.name} ${dependency.version}`)
            .join(", ")}.
        </p>
      {/if}
      <details class="mt-1 text-xs">
        <summary class="cursor-pointer text-base-content/70"
          >Technical details</summary
        >
        <p class="mt-1">
          Digest
          <span class="trellis-identifier">{pulled.published.digest}</span>
        </p>
      </details>
    </div>
  {/if}
</div>
