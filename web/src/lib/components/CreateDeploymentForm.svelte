<script lang="ts">
  import { apis } from "trellis-web-generated";
  import {
    base64urlDecode,
    base64urlEncode,
    publicKeyBase64urlFromSeed,
  } from "@oatscenter/trellis/auth/browser";
  import { ulid } from "ulid";
  import { onMount, tick } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";

  let { kind }: { kind: "service" | "device" } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const noun = $derived(kind === "service" ? "service" : "device type");
  let title = $state("");
  // The IDL file is the exact source closure the service was built from (the
  // JSON Auth recompiles). Without it a service is added as a claim: whatever
  // first connects with its key presents its own IDL for approval.
  let idl = $state("");
  let chosenParticipant = $state("");
  let keyMode = $state<"create" | "existing">("create");
  let publicKey = $state("");
  let busy = $state(false);
  let uncertain = $state(false);
  let disposed = false;
  let error = $state<string | null>(null);
  // Auth refused because another service already answers an API this IDL
  // implements; adding it anyway needs the administrator's explicit consent.
  let sharing = $state<string | null>(null);
  let sharingUnderstood = $state(false);
  let added = $state<{ title: string; id: string; href: string } | null>(null);
  // A seed generated here. It never leaves the browser: only its public key is
  // sent, and closing the dialog drops the last reference to it.
  let seed = $state<string | null>(null);
  let seedDialog: HTMLDialogElement | undefined = $state();

  type ParsedIdl =
    | { ok: true; evidence: unknown; participants: string[] }
    | { ok: false; message: string };
  const parsed = $derived.by((): ParsedIdl | null => {
    if (!idl.trim()) return null;
    let value: unknown;
    try {
      value = JSON.parse(idl);
    } catch {
      return { ok: false, message: "This isn't an IDL file." };
    }
    if (typeof value !== "object" || value === null)
      return { ok: false, message: "This isn't an IDL file." };
    const { rootPackage, packages } = value as {
      rootPackage?: unknown;
      packages?: unknown;
    };
    const root = Array.isArray(packages)
      ? (packages as { name?: unknown; source?: unknown }[]).find(
          (entry) => entry?.name === rootPackage,
        )
      : undefined;
    if (typeof rootPackage !== "string" || typeof root?.source !== "string")
      return { ok: false, message: "This isn't an IDL file." };
    // The source is presentation-canonical, so every top-level participant
    // starts a line with its kind and name.
    const pattern = new RegExp(`^${kind} ([A-Za-z_][A-Za-z0-9_]*) \\{$`, "gm");
    const participants = [...root.source.matchAll(pattern)].map(
      (match) => `${rootPackage}.${match[1]}`,
    );
    if (participants.length === 0)
      return {
        ok: false,
        message: `This IDL doesn't declare a ${kind}.`,
      };
    return { ok: true, evidence: value, participants };
  });
  const participantId = $derived(
    parsed?.ok
      ? parsed.participants.length === 1
        ? parsed.participants[0]
        : parsed.participants.includes(chosenParticipant)
          ? chosenParticipant
          : ""
      : "",
  );
  const publicKeyError = $derived.by(() => {
    const value = publicKey.trim();
    if (!value) return null;
    try {
      if (
        /^[A-Za-z0-9_-]{43}$/.test(value) &&
        base64urlDecode(value).length === 32
      )
        return null;
    } catch {
      // Falls through to the message below.
    }
    return "A public key is 43 letters, digits, - or _.";
  });
  const withIdl = $derived(kind === "device" || parsed !== null);
  const ready = $derived(
    !busy &&
      !uncertain &&
      title.trim() !== "" &&
      (withIdl ? parsed?.ok === true && participantId !== "" : true) &&
      (sharing === null || sharingUnderstood) &&
      (kind === "device" ||
        keyMode === "create" ||
        (publicKey.trim() !== "" && publicKeyError === null)),
  );

  async function chooseFile(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    idl = await file.text();
    input.value = "";
  }

  async function submit(): Promise<void> {
    if (!ready || added) return;
    if (
      withIdl
        ? !canPerform(authority.authority, "deploymentsCreate") ||
          (kind === "service" &&
            !canPerform(authority.authority, "serviceInstancesProvision"))
        : !canPerform(authority.authority, "serviceClaimsCreate")
    )
      return;
    busy = true;
    error = null;
    const name = title.trim();
    let generated: string | null = null;
    let identityPublicKey: string | null = null;
    if (kind === "service") {
      if (keyMode === "create") {
        const bytes = crypto.getRandomValues(new Uint8Array(32));
        generated = base64urlEncode(bytes);
        identityPublicKey = publicKeyBase64urlFromSeed(bytes);
        bytes.fill(0);
      } else identityPublicKey = publicKey.trim();
    }
    try {
      if (!withIdl && identityPublicKey) {
        const response = await trellis
          .serviceClaimsCreate({
            title: name,
            instanceTitle: name,
            identityPublicKey,
            idempotencyKey: ulid(),
          })
          .orThrow();
        if (disposed) return;
        added = {
          title: name,
          id: response.claim.claimId,
          href: resolve("/admin/services/[deploymentId]", {
            deploymentId: response.claim.claimId,
          }),
        };
      } else if (parsed?.ok) {
        const input = apis.auth.API.actions[
          "rpc:Deployments.Create"
        ].input.decode({
          title: name,
          kind,
          participantId,
          packageEvidence: parsed.evidence,
          ...(sharing && sharingUnderstood
            ? { acknowledgeSharedApis: true }
            : {}),
          idempotencyKey: ulid(),
        });
        const response = await trellis.deploymentsCreate(input).orThrow();
        if (disposed) return;
        const deploymentId = response.deployment.deploymentId;
        added = {
          title: response.deployment.title,
          id: deploymentId,
          href:
            kind === "service"
              ? resolve("/admin/services/[deploymentId]", { deploymentId })
              : resolve("/admin/devices"),
        };
        if (identityPublicKey) {
          try {
            await trellis
              .serviceInstancesProvision({
                deploymentId,
                title: name,
                identityPublicKey,
                idempotencyKey: ulid(),
              })
              .orThrow();
          } catch (cause) {
            if (disposed) return;
            generated = null;
            error = `${name} was added, but its key wasn't: ${projectConsoleError(cause).message} Add an instance from the service's page.`;
            return;
          }
        }
      }
      if (disposed || !generated) return;
      seed = generated;
      await tick();
      seedDialog?.showModal();
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
        ? `Couldn't tell whether the ${noun} was added. Check the list before adding it again.`
        : message;
    } finally {
      if (!disposed) busy = false;
    }
  }

  function closeSeed(): void {
    seed = null;
    if (seedDialog?.open) seedDialog.close();
  }

  onMount(() => () => {
    disposed = true;
    seed = null;
  });
</script>

<section class="max-w-3xl space-y-5">
  <PageToolbar
    eyebrow={kind === "service" ? "Services" : "Devices"}
    title={kind === "service" ? "Add a service" : "New device type"}
    description={kind === "service"
      ? "Give it a name and a key. Its IDL is optional."
      : "A device type describes software that many devices run."}
    >{#snippet actions()}<a
        class="btn btn-ghost"
        href={resolve(
          kind === "service" ? "/admin/services" : "/admin/devices",
        )}>Back to {kind === "service" ? "services" : "devices"}</a
      >{/snippet}</PageToolbar
  >
  {#if error}<Notice variant={uncertain ? "warning" : "error"}>{error}</Notice
    >{/if}
  {#if added}
    <div class="trellis-section p-5">
      <p class="font-medium">{added.title} was added.</p>
      <details class="mt-2 text-sm">
        <summary class="cursor-pointer text-base-content/70"
          >Technical details</summary
        >
        <p class="mt-2 text-xs">
          ID <span class="trellis-identifier" data-testid="created-id"
            >{added.id}</span
          >
        </p>
      </details>
      <a class="btn btn-primary mt-4" href={added.href}
        >{kind === "service" ? "Open service" : "Go to devices"}</a
      >
    </div>
  {:else}
    <form
      class="trellis-section trellis-form p-4 sm:p-5"
      onsubmit={(event) => {
        event.preventDefault();
        void submit();
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
        <label class="trellis-field-label" for="deployment-idl"
          >{kind === "service" ? "Service IDL (optional)" : "Device IDL"}</label
        ><textarea
          id="deployment-idl"
          class="textarea min-h-32 w-full font-mono text-xs"
          bind:value={idl}
          required={kind === "device"}
          spellcheck="false"
          disabled={busy || uncertain}
          aria-describedby="deployment-idl-help"
          aria-invalid={parsed?.ok === false}
        ></textarea>
        <div class="flex flex-wrap items-center gap-2">
          <input
            type="file"
            class="file-input file-input-sm w-full sm:w-auto"
            accept=".json,application/json"
            aria-label="Choose an IDL file"
            disabled={busy || uncertain}
            onchange={(event) => void chooseFile(event)}
          />
        </div>
        <span id="deployment-idl-help" class="trellis-field-help"
          >{#if parsed?.ok === false}<span class="text-error"
              >{parsed.message}</span
            >{:else if parsed?.ok && parsed.participants.length === 1}Adds {parsed
              .participants[0]}.{:else if kind === "service"}Paste or choose the
            service's IDL file. Without it, the service introduces itself when
            it first connects and you approve what it asks for.{:else}Paste or
            choose the device's IDL file.{/if}</span
        >
      </div>
      {#if parsed?.ok && parsed.participants.length > 1}
        <label class="trellis-field"
          ><span class="trellis-field-label">Which {kind}</span><select
            class="select w-full"
            bind:value={chosenParticipant}
            required
            disabled={busy || uncertain}
          >
            <option value="" disabled>Choose one</option>
            {#each parsed.participants as participant (participant)}
              <option value={participant}
                >{participant.slice(participant.lastIndexOf(".") + 1)}</option
              >
            {/each}
          </select></label
        >
      {/if}
      {#if kind === "service"}
        <fieldset class="trellis-field" disabled={busy || uncertain}>
          <legend class="trellis-field-label mb-1.5">Key</legend>
          <div class="grid gap-2 sm:grid-cols-2">
            {#each [{ value: "create", label: "Create a key", help: "You copy it once, after adding." }, { value: "existing", label: "I already have a key", help: "Paste its public key." }] as const as option (option.value)}
              <label
                class={[
                  "flex cursor-pointer gap-3 rounded-box border p-3",
                  keyMode === option.value
                    ? "border-primary bg-primary/5"
                    : "border-base-300",
                ]}
                ><input
                  type="radio"
                  class="radio radio-sm mt-0.5"
                  name="key-mode"
                  value={option.value}
                  bind:group={keyMode}
                /><span
                  ><span class="block font-medium">{option.label}</span><span
                    class="block text-sm text-base-content/60"
                    >{option.help}</span
                  ></span
                ></label
              >
            {/each}
          </div>
        </fieldset>
        {#if keyMode === "existing"}
          <div class="trellis-field">
            <label class="trellis-field-label" for="deployment-public-key"
              >Public key</label
            ><input
              id="deployment-public-key"
              class="input w-full font-mono"
              bind:value={publicKey}
              autocomplete="off"
              spellcheck="false"
              aria-describedby="deployment-public-key-help"
              aria-invalid={publicKeyError !== null}
              required
              disabled={busy || uncertain}
            /><span id="deployment-public-key-help" class="trellis-field-help"
              >{#if publicKeyError}<span class="text-error"
                  >{publicKeyError}</span
                >{:else}The service keeps the matching private key.{/if}</span
            >
          </div>
        {/if}
      {/if}
      {#if sharing}
        <div class="alert alert-warning block space-y-2" role="alert">
          <p class="font-semibold">
            Another service already answers an API this one provides.
          </p>
          <p class="text-sm">
            Adding it makes both services answer the same requests. Each request
            goes to one of them at random, so each sees only some requests, they
            keep separate data, and callers can't tell which one answered. To
            add capacity or move a service, add instances to the existing
            service instead.
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
          class="btn btn-primary w-full sm:w-auto"
          disabled={!ready}
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

<dialog
  bind:this={seedDialog}
  class="modal modal-bottom sm:modal-middle"
  aria-labelledby="seed-dialog-title"
  oncancel={(event) => event.preventDefault()}
  onclose={() => (seed = null)}
>
  {#if seed}
    <div class="modal-box">
      <h3 id="seed-dialog-title" class="text-lg font-semibold">
        Save the service's key
      </h3>
      <p class="mt-2 text-sm text-base-content/70">
        Start the service with this key. Trellis doesn't keep it, and you won't
        see it again after you close this.
      </p>
      <div class="mt-3 flex items-start gap-2">
        <code
          class="trellis-identifier block min-w-0 flex-1 break-all rounded-field bg-base-200 p-3 select-all"
          aria-label="Service key">{seed}</code
        >
        <CopyButton value={seed} label="Copy key" />
      </div>
      <div class="modal-action">
        <button
          type="button"
          class="btn btn-primary w-full sm:w-auto"
          onclick={closeSeed}>I've saved it</button
        >
      </div>
    </div>
  {/if}
</dialog>
