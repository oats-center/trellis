<script lang="ts">
  import { type apis } from "$lib/trellis-apis";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { onMount } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { resolve } from "$lib/console_paths";
  import { formatDate } from "$lib/format";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";

  type Registry = apis.auth.PackageRegistriesListOutput["registries"][number];

  // Registries are where Trellis pulls published packages from when a
  // service is added or updated from one. A private registry's password is
  // sealed by Trellis and never shown again.
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  let registries = $state.raw<Registry[] | null>(null);
  let error = $state<string | null>(null);
  let busy = $state(false);
  let name = $state("");
  let prefix = $state("");
  let signIn = $state(false);
  let username = $state("");
  let secret = $state("");
  let confirmationModal: ConfirmationModal | undefined = $state();
  let disposed = false;

  const canChange = $derived(
    canPerform(authority.authority, "packageRegistriesPut"),
  );
  const ready = $derived(
    !busy &&
      name.trim() !== "" &&
      prefix.trim() !== "" &&
      (!signIn || (username.trim() !== "" && secret !== "")),
  );

  async function load(): Promise<void> {
    try {
      const listed = await trellis.packageRegistriesList({}).orThrow();
      if (!disposed) registries = listed.registries;
    } catch (cause) {
      if (!disposed) error = projectConsoleError(cause).message;
    }
  }

  async function save(): Promise<void> {
    if (!ready || !canChange) return;
    busy = true;
    error = null;
    try {
      await trellis
        .packageRegistriesPut({
          name: name.trim(),
          prefix: prefix.trim(),
          ...(signIn
            ? { credentials: { username: username.trim(), secret } }
            : {}),
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (disposed) return;
      name = "";
      prefix = "";
      username = "";
      secret = "";
      signIn = false;
      await load();
    } catch (cause) {
      if (!disposed) error = projectConsoleError(cause).message;
    } finally {
      if (!disposed) busy = false;
    }
  }

  async function remove(registry: Registry): Promise<void> {
    if (busy) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Remove this registry?",
      message:
        "Trellis stops pulling packages from it. Services already added keep their builds.",
      confirmLabel: "Remove",
      targetLabel: "Registry",
      targetName: registry.name,
    });
    if (!confirmed || disposed) return;
    busy = true;
    error = null;
    try {
      await trellis
        .packageRegistriesRemove({
          name: registry.name,
          idempotencyKey: codecs.ulid.decode(ulid()),
        })
        .orThrow();
      if (!disposed) await load();
    } catch (cause) {
      if (!disposed) error = projectConsoleError(cause).message;
    } finally {
      if (!disposed) busy = false;
    }
  }

  onMount(() => {
    void load();
    return () => {
      disposed = true;
    };
  });
</script>

<section class="max-w-3xl space-y-5">
  <PageToolbar
    eyebrow="Services"
    title="Package registries"
    description="Where Trellis pulls published service and device packages from."
    >{#snippet actions()}<a
        class="btn btn-ghost"
        href={resolve("/admin/services")}>Back to services</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant="error">{error}</Notice>{/if}
  {#if registries === null}
    {#if !error}<LoadingState label="Loading registries" />{/if}
  {:else}
    <div class="trellis-section">
      {#if registries.length === 0}
        <p class="p-5 text-sm text-base-content/70">No registries yet.</p>
      {:else}
        <table class="table">
          <thead>
            <tr>
              <th class="trellis-primary">Name</th>
              <th class="hidden sm:table-cell">Address</th>
              <th class="hidden sm:table-cell">Signs in as</th>
              <th><span class="sr-only">Actions</span></th>
            </tr>
          </thead>
          <tbody>
            {#each registries as registry (registry.name)}
              <tr>
                <td class="trellis-primary"
                  ><span class="font-medium">{registry.name}</span>
                  <span
                    class="block font-mono text-xs text-base-content/60 sm:hidden"
                    >{registry.prefix}</span
                  ></td
                >
                <td class="hidden font-mono text-sm sm:table-cell"
                  >{registry.prefix}</td
                >
                <td class="hidden text-sm sm:table-cell"
                  >{registry.username ?? "Anyone (public)"}
                  <span class="block text-xs text-base-content/60"
                    >Changed {formatDate(registry.updatedAt)}</span
                  ></td
                >
                <td class="text-right"
                  >{#if canPerform(authority.authority, "packageRegistriesRemove")}<button
                      class="btn btn-ghost btn-sm text-error"
                      disabled={busy}
                      aria-label={`Remove ${registry.name}`}
                      onclick={() => void remove(registry)}>Remove</button
                    >{/if}</td
                >
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </div>
  {/if}
  {#if canChange}
    <form
      class="trellis-section trellis-form p-4 sm:p-5"
      aria-label="Add a registry"
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      <h2 class="text-base font-semibold">Add a registry</h2>
      <div class="grid gap-3 sm:grid-cols-2">
        <label class="trellis-field"
          ><span class="trellis-field-label">Name</span><input
            class="input w-full"
            bind:value={name}
            autocomplete="off"
            required
            disabled={busy}
          /></label
        >
        <label class="trellis-field"
          ><span class="trellis-field-label">Address</span><input
            class="input w-full font-mono"
            bind:value={prefix}
            autocomplete="off"
            spellcheck="false"
            placeholder="ghcr.io/your-org"
            required
            disabled={busy}
          /></label
        >
      </div>
      <label class="flex items-center gap-2 text-sm"
        ><input
          type="checkbox"
          class="checkbox checkbox-sm"
          bind:checked={signIn}
          disabled={busy}
        />It's private; sign in to it</label
      >
      {#if signIn}
        <div class="grid gap-3 sm:grid-cols-2">
          <label class="trellis-field"
            ><span class="trellis-field-label">User</span><input
              class="input w-full"
              bind:value={username}
              autocomplete="off"
              required
              disabled={busy}
            /></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">Password or token</span><input
              class="input w-full"
              type="password"
              bind:value={secret}
              autocomplete="new-password"
              required
              disabled={busy}
            /></label
          >
        </div>
      {/if}
      <span class="trellis-field-help"
        >A registry with the same name is replaced.</span
      >
      <div class="trellis-action-row">
        <button class="btn btn-primary" type="submit" disabled={!ready}
          >{busy ? "Saving…" : "Add registry"}</button
        >
      </div>
    </form>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
