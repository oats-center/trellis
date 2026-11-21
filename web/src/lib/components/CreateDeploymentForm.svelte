<script lang="ts">
  import { type apis } from "$lib/trellis-apis";
  import {
    base64urlDecode,
    base64urlEncode,
    publicKeyBase64urlFromSeed,
  } from "@oatscenter/trellis/auth/browser";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { onMount, tick } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { resolve } from "$lib/console_paths";
  import {
    freeSettings,
    parseSetting,
    type FreeSetting,
  } from "$lib/console/settings.ts";
  import type { JsonValue } from "@oatscenter/trellis/generated";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import FreeSettingRows from "$lib/components/FreeSettingRows.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import PublishedPackagePicker from "$lib/components/PublishedPackagePicker.svelte";
  import SettingInput from "$lib/components/SettingInput.svelte";

  let { kind }: { kind: "service" | "device" } = $props();
  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const noun = $derived(kind === "service" ? "service" : "device type");
  let title = $state("");
  // The IDL is the service's own trellis.toml and .trellis files. Auth builds
  // the evidence from them, taking dependencies from the packages it holds;
  // the user supplies files for any it lacks. Without an IDL a service is added
  // as a claim: whatever first connects with its key presents its own IDL for
  // approval.
  type PackageFiles = {
    manifest?: string;
    files: { path: string; text: string }[];
    names: string[];
  };
  let root = $state<PackageFiles | null>(null);
  // Or the IDL comes from a release published to a registry Trellis pulls
  // from: Auth pulls it, checks its digests, and builds it from exactly the
  // dependencies it was published with.
  let from = $state<"files" | "published">("files");
  let published = $state.raw<apis.auth.PackagesPullOutput | null>(null);
  let packageName = $state("");
  let packageVersion = $state("");
  let extras = $state<Record<string, PackageFiles>>({});
  // Dependency changes run one at a time, each against the packages Auth has
  // already accepted, so a refusal is blamed on the package that caused it and
  // a selection made while another resolves is never lost. `epoch` changes with
  // the IDL; a change queued for an earlier IDL is skipped.
  let changes: Promise<void> = Promise.resolve();
  let rootPicks = 0;
  let queued = 0;
  let epoch = 0;
  type Missing = apis.auth.PackagesResolveOutput["missing"][number];
  // Every package the IDL needed that Trellis doesn't have, with the ones the
  // administrator supplied. A row stays while its files are in use or a
  // selection for it failed, so it can be retried or removed.
  let wanted = $state.raw<Missing[]>([]);
  // Why the last selection for a package wasn't used, by package name.
  let extraErrors = $state<Record<string, string>>({});
  let resolution = $state<apis.auth.PackagesResolveOutput | null>(null);
  let resolving = $state(false);
  let resolveError = $state<string | null>(null);
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
  // Set when Auth asks to confirm a webhook that checks nothing.
  let unauthenticated = $state(false);
  let unauthenticatedUnderstood = $state(false);
  let added = $state<{ title: string; id: string; href: string } | null>(null);
  // A seed generated here. It never leaves the browser: only its public key is
  // sent, and closing the dialog drops the last reference to it.
  let seed = $state<string | null>(null);
  let seedDialog: HTMLDialogElement | undefined = $state();
  // Settings saved with the new service: typed by its IDL when it has one,
  // otherwise named by hand.
  let drafts = $state<Record<string, string>>({});
  let revealed = $state<Record<string, boolean>>({});
  let freeRows = $state<FreeSetting[]>([]);

  const participants = $derived(
    (resolution?.participants ?? [])
      .filter((participant) => participant.kind === kind)
      .map((participant) => participant.participantId),
  );
  const participantId = $derived(
    participants.length === 1
      ? participants[0]
      : participants.includes(chosenParticipant)
        ? chosenParticipant
        : "",
  );
  const evidence = $derived(resolution?.packageEvidence);
  const declaredSettings = $derived(
    resolution?.participants.find(
      (participant) => participant.participantId === participantId,
    )?.config ?? [],
  );
  const idlProblem = $derived.by((): string | null => {
    if (from === "published")
      return published && participants.length === 0
        ? `This package doesn't declare a ${kind}.`
        : null;
    if (!root) return null;
    if (root.files.length === 0) return "Choose its .trellis files too.";
    if (!root.manifest && (!packageName.trim() || !packageVersion.trim()))
      return "Without trellis.toml, give the package's name and version.";
    if (resolveError) return resolveError;
    if (!resolution) return null;
    if (resolution.unknownImports.length > 0)
      return `Add its trellis.toml. It imports from ${resolution.unknownImports.join(", ")}, which Trellis doesn't have.`;
    if (resolution.missing.length > 0) return null;
    if (participants.length === 0) return `This IDL doesn't declare a ${kind}.`;
    return null;
  });
  const usedPackages = $derived(
    (resolution?.dependencies ?? []).map(
      (dependency) =>
        `${dependency.name} ${dependency.version} (${
          dependency.origin === "builtin"
            ? "built in"
            : dependency.origin === "supplied"
              ? "your files"
              : dependency.origin === "registry"
                ? "from its registry"
                : "installed"
        }${dependency.otherVersions.length > 0 ? `; highest of ${dependency.otherVersions.length + 1} that match` : ""})`,
    ),
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
  const withIdl = $derived(
    kind === "device" || root !== null || from === "published",
  );
  const ready = $derived(
    !busy &&
      !uncertain &&
      !resolving &&
      title.trim() !== "" &&
      (withIdl ? evidence !== undefined && participantId !== "" : true) &&
      (sharing === null || sharingUnderstood) &&
      (!unauthenticated || unauthenticatedUnderstood) &&
      (kind === "device" ||
        keyMode === "create" ||
        (publicKey.trim() !== "" && publicKeyError === null)),
  );

  /** Read a package's trellis.toml and .trellis files from a file selection. */
  async function readPackage(
    list: FileList | null | undefined,
  ): Promise<PackageFiles | null> {
    const picked = [...(list ?? [])]
      .map((file) => ({ file, path: file.webkitRelativePath || file.name }))
      .filter(
        ({ path }) =>
          path.endsWith(".trellis") ||
          path === "trellis.toml" ||
          path.endsWith("/trellis.toml"),
      );
    if (picked.length === 0) return null;
    // In a folder, the shallowest trellis.toml is the package's; source paths
    // are relative to it.
    const [manifest] = picked
      .filter(({ path }) => !path.endsWith(".trellis"))
      .sort((a, b) => a.path.split("/").length - b.path.split("/").length);
    const base = manifest ? manifest.path.slice(0, -"trellis.toml".length) : "";
    const sources = picked.filter(
      ({ path }) => path.endsWith(".trellis") && path.startsWith(base),
    );
    return {
      manifest: manifest ? await manifest.file.text() : undefined,
      files: await Promise.all(
        sources.map(async ({ file, path }) => ({
          path: path.slice(base.length),
          text: await file.text(),
        })),
      ),
      names: [...(manifest ? [manifest] : []), ...sources].map(
        ({ path }) => path,
      ),
    };
  }

  /** Switch between IDL files and a published package, starting over. */
  function chooseFrom(next: "files" | "published"): void {
    from = next;
    root = null;
    published = null;
    resolution = null;
    resolveError = null;
    rootPicks++;
    epoch++;
    drafts = {};
    revealed = {};
    extras = {};
    wanted = [];
    extraErrors = {};
    sharing = null;
  }

  /** Take a release the picker pulled as this form's IDL. */
  function usePublished(pulled: apis.auth.PackagesPullOutput | null): void {
    published = pulled;
    drafts = {};
    revealed = {};
    sharing = null;
    resolution = pulled && {
      packageEvidence: pulled.packageEvidence,
      participants: pulled.participants,
      dependencies: pulled.dependencies,
      missing: [],
      unknownImports: [],
      rejected: [],
    };
  }

  async function chooseRoot(event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    // Reading files takes time; a later choice of IDL wins over this one.
    const pick = ++rootPicks;
    const chosen = await readPackage(input.files);
    input.value = "";
    if (disposed || pick !== rootPicks) return;
    root = chosen;
    epoch++;
    drafts = {};
    revealed = {};
    extras = {};
    wanted = [];
    extraErrors = {};
    sharing = null;
    await queue(() => resolvePackages());
  }

  /** Run a dependency change after every change queued before it. */
  function queue(change: () => Promise<void>): Promise<void> {
    const queuedIn = epoch;
    queued++;
    resolving = true;
    const run = async () => {
      try {
        if (!disposed && queuedIn === epoch) await change();
      } finally {
        queued--;
        if (!disposed && queued === 0) resolving = false;
      }
    };
    changes = changes.then(run, run);
    return changes;
  }

  // A selection for one package is used only once Auth accepts it; a refused
  // one leaves the IDL and every other package as they were.
  async function chooseExtra(name: string, event: Event): Promise<void> {
    const input = event.currentTarget as HTMLInputElement;
    // Files chosen for one IDL never reach a draft for another, even when
    // the IDL changes while they are still being read.
    const chosenFor = epoch;
    const chosen = await readPackage(input.files);
    input.value = "";
    if (disposed || chosenFor !== epoch) return;
    if (!chosen?.manifest || chosen.files.length === 0) {
      extraErrors = {
        ...extraErrors,
        [name]: !chosen
          ? `Choose the trellis.toml and .trellis files for ${name}.`
          : !chosen.manifest
            ? `Add the trellis.toml for ${name} with its files.`
            : `Add the .trellis files for ${name} with its trellis.toml.`,
      };
      return;
    }
    // `extras` is read when the change runs, after every earlier one settled.
    await queue(() => resolvePackages({ ...extras, [name]: chosen }, name));
  }

  function removeExtra(name: string): void {
    extraErrors = without(extraErrors, name);
    void queue(async () => {
      if (name in extras) await resolvePackages(without(extras, name));
    });
  }

  function without<T>(record: Record<string, T>, name: string) {
    return Object.fromEntries(
      Object.entries(record).filter(([key]) => key !== name),
    );
  }

  /**
   * Ask Auth to build the evidence from the IDL and `supplied` packages, or to
   * name what it still needs. When `attempted` names the package just chosen,
   * a refusal is shown on its row and the previous result stands.
   */
  async function resolvePackages(
    supplied: Record<string, PackageFiles> = extras,
    attempted?: string,
  ): Promise<void> {
    const resolvingFor = epoch;
    if (!attempted) resolution = null;
    resolveError = null;
    const current = root;
    if (
      !current ||
      current.files.length === 0 ||
      (!current.manifest && (!packageName.trim() || !packageVersion.trim()))
    )
      return;
    const packages = Object.values(supplied);
    try {
      const response = await trellis
        .packagesResolve({
          root: {
            files: current.files,
            ...(current.manifest
              ? { manifest: current.manifest }
              : { name: packageName.trim(), version: packageVersion.trim() }),
          },
          ...(packages.length > 0
            ? {
                packages: packages.map((files) => ({
                  manifest: files.manifest,
                  files: files.files,
                })),
              }
            : {}),
        })
        .orThrow();
      if (disposed || resolvingFor !== epoch) return;
      if (response.rejected.length > 0) {
        // Auth names each supplied package that doesn't compile by its place
        // in the request, so the error lands on the row that supplied the
        // files whatever name their trellis.toml gives. Those files are
        // dropped and the rest resolved again.
        const rows = Object.keys(supplied);
        const rejected = new Map(
          response.rejected.flatMap((item) => {
            const row = rows[item.index];
            return row === undefined ? [] : [[row, item.reason] as const];
          }),
        );
        if (rejected.size === 0) {
          // Nothing to drop, so asking again would get the same answer.
          resolveError = response.rejected
            .map((item) => `${item.name}: ${item.reason}`)
            .join(" ");
          return;
        }
        extraErrors = { ...extraErrors, ...Object.fromEntries(rejected) };
        await resolvePackages(
          Object.fromEntries(
            Object.entries(supplied).filter(([name]) => !rejected.has(name)),
          ),
          attempted && !rejected.has(attempted) ? attempted : undefined,
        );
        return;
      }
      // Only files Auth used for the package their row is for are kept:
      // anything else would ride along in every later request, and could
      // stand in for a package its row isn't for.
      const used = new Set(
        response.dependencies
          .filter((dependency) => dependency.origin === "supplied")
          .map((dependency) => dependency.name),
      );
      const kept = Object.fromEntries(
        Object.entries(supplied).filter(([name]) => used.has(name)),
      );
      const dropped = Object.keys(supplied).filter((name) => !(name in kept));
      if (dropped.length > 0) {
        extraErrors = {
          ...extraErrors,
          ...Object.fromEntries(
            dropped.map((name) => [
              name,
              `These files don't provide a ${name} that matches.`,
            ]),
          ),
        };
        // The answer counted the dropped files, so it may say a package is
        // satisfied when nothing kept provides it; ask again without them.
        await resolvePackages(
          kept,
          attempted && !dropped.includes(attempted) ? attempted : undefined,
        );
        return;
      }
      resolution = response;
      extras = kept;
      if (attempted) extraErrors = without(extraErrors, attempted);
      wanted = [
        ...response.missing,
        ...wanted.filter(
          (entry) =>
            entry.name in supplied &&
            !response.missing.some((item) => item.name === entry.name),
        ),
      ];
    } catch (cause) {
      if (disposed || resolvingFor !== epoch) return;
      const message = projectConsoleError(cause).message;
      if (attempted) extraErrors = { ...extraErrors, [attempted]: message };
      else resolveError = message;
    }
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
    let settings: {
      values: Record<string, JsonValue>;
      secrets: Record<string, JsonValue>;
    };
    try {
      settings = withIdl ? declaredValues() : freeSettings(freeRows);
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
      return;
    }
    busy = true;
    error = null;
    const name = title.trim();
    let generated: string | null = null;
    // Settings go in right after the service exists, before any instance of
    // it can connect and read them.
    const saveSettings = async (
      target: apis.auth.ConfigSetInput["target"],
    ): Promise<void> => {
      if (
        Object.keys(settings.values).length === 0 &&
        Object.keys(settings.secrets).length === 0
      )
        return;
      try {
        await trellis
          .configSet({
            target,
            values: settings.values,
            secrets: settings.secrets,
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow();
      } catch (cause) {
        if (disposed) return;
        error = `${name} was added, but its settings weren't: ${projectConsoleError(cause).message} Set them on its page.`;
      }
    };
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
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow();
        if (disposed) return;
        await saveSettings({ claimId: response.claim.claimId });
        if (disposed) return;
        added = {
          title: name,
          id: response.claim.claimId,
          href: resolve("/admin/services/[deploymentId]", {
            deploymentId: response.claim.claimId,
          }),
        };
      } else if (evidence) {
        const response = await trellis
          .deploymentsCreate({
            title: name,
            kind,
            participantId,
            packageEvidence: evidence,
            ...(sharing && sharingUnderstood
              ? { acknowledgeSharedApis: true }
              : {}),
            ...(unauthenticated && unauthenticatedUnderstood
              ? { acknowledgeUnauthenticatedWebhooks: true }
              : {}),
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow();
        if (disposed) return;
        const deploymentId = response.deployment.deploymentId;
        await saveSettings({ deploymentId });
        if (disposed) return;
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
                idempotencyKey: codecs.ulid.decode(ulid()),
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
      if (
        !uncertain &&
        message.includes("acknowledgeUnauthenticatedWebhooks")
      ) {
        unauthenticated = true;
        unauthenticatedUnderstood = false;
        return;
      }
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

  /** What was typed for the IDL's settings; untouched ones are left out. */
  function declaredValues(): {
    values: Record<string, JsonValue>;
    secrets: Record<string, JsonValue>;
  } {
    const values: Record<string, JsonValue> = {};
    const secrets: Record<string, JsonValue> = {};
    for (const field of declaredSettings) {
      const text = drafts[field.name];
      if (text === undefined || text === "") continue;
      (field.secret ? secrets : values)[field.name] = parseSetting(field, text);
    }
    return { values, secrets };
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
      <fieldset class="trellis-field" disabled={busy || uncertain}>
        <legend class="trellis-field-label mb-1.5">
          Where its IDL comes from
        </legend>
        <div class="flex flex-wrap gap-4">
          {#each [{ value: "files", label: "From IDL files" }, { value: "published", label: "From a published package" }] as const as option (option.value)}
            <label class="flex cursor-pointer items-center gap-2 text-sm"
              ><input
                type="radio"
                class="radio radio-sm"
                name="idl-from"
                value={option.value}
                checked={from === option.value}
                onchange={() => chooseFrom(option.value)}
              />{option.label}</label
            >
          {/each}
        </div>
      </fieldset>
      {#if from === "published"}
        <PublishedPackagePicker
          disabled={busy || uncertain}
          onpulled={usePublished}
        />
        {#if idlProblem}<p class="text-sm text-error">{idlProblem}</p>{/if}
      {:else}
        <div class="trellis-field">
          <label class="trellis-field-label" for="deployment-idl"
            >{kind === "service"
              ? "Service IDL (optional)"
              : "Device IDL"}</label
          >
          <div class="flex flex-wrap items-center gap-2">
            <input
              id="deployment-idl"
              type="file"
              multiple
              class="file-input file-input-sm w-full sm:w-auto"
              accept=".trellis,.toml"
              required={kind === "device" && root === null}
              disabled={busy || uncertain}
              aria-describedby="deployment-idl-help"
              aria-invalid={idlProblem !== null}
              onchange={(event) => void chooseRoot(event)}
            />
            <label
              class={[
                "btn btn-outline btn-sm",
                (busy || uncertain) && "btn-disabled",
              ]}
              >Choose a folder<input
                type="file"
                class="sr-only"
                webkitdirectory
                disabled={busy || uncertain}
                onchange={(event) => void chooseRoot(event)}
              /></label
            >
          </div>
          {#if root}
            <ul
              class="font-mono text-xs text-base-content/70"
              aria-label="Chosen IDL files"
            >
              {#each root.names as name (name)}<li>{name}</li>{/each}
            </ul>
            <button
              type="button"
              class="btn btn-ghost btn-xs self-start"
              disabled={busy || uncertain}
              onclick={() => {
                root = null;
                rootPicks++;
                epoch++;
                extras = {};
                wanted = [];
                extraErrors = {};
                sharing = null;
                void queue(() => resolvePackages());
              }}>Remove IDL</button
            >
          {/if}
          <span id="deployment-idl-help" class="trellis-field-help"
            >{#if idlProblem}<span class="text-error">{idlProblem}</span
              >{:else if resolving}Checking its IDL…{:else if evidence}{#if participants.length === 1}Adds
                {participants[0]}.{/if}
              {#if usedPackages.length > 0}Uses {usedPackages.join(
                  ", ",
                )}.{/if}{:else if resolution}Add the packages below.{:else if kind === "service"}Choose
              its trellis.toml and .trellis files, or their folder. Without
              them, the service introduces itself when it first connects and you
              approve what it asks for.{:else}Choose its trellis.toml and
              .trellis files, or their folder.{/if}</span
          >
        </div>
      {/if}
      {#if root && !root.manifest}
        <div class="grid gap-3 sm:grid-cols-2">
          <label class="trellis-field"
            ><span class="trellis-field-label">Package name</span><input
              class="input w-full font-mono"
              bind:value={packageName}
              autocomplete="off"
              spellcheck="false"
              required
              disabled={busy || uncertain}
              onchange={() => void queue(() => resolvePackages())}
            /></label
          >
          <label class="trellis-field"
            ><span class="trellis-field-label">Package version</span><input
              class="input w-full font-mono"
              bind:value={packageVersion}
              autocomplete="off"
              spellcheck="false"
              placeholder="1.0.0"
              required
              disabled={busy || uncertain}
              onchange={() => void queue(() => resolvePackages())}
            /></label
          >
        </div>
      {/if}
      {#if root && wanted.length > 0}
        <section class="trellis-field" aria-labelledby="missing-packages">
          <h3 id="missing-packages" class="trellis-field-label">
            Packages Trellis doesn't have
          </h3>
          <ul class="space-y-2">
            {#each wanted as missing (missing.name)}
              <li class="rounded-box border border-base-300 p-3">
                <p class="text-sm">
                  <span class="font-mono">{missing.name}</span>
                  <span class="text-base-content/60"
                    >{missing.requirements.length > 0
                      ? missing.requirements.join(", ")
                      : "any version"}</span
                  >
                </p>
                {#if missing.availableVersions.length > 0}
                  <p class="text-xs text-base-content/60">
                    {missing.availableVersions.join(", ")}
                    {missing.availableVersions.length === 1
                      ? "doesn't"
                      : "don't"} match.
                  </p>
                {/if}
                <div class="mt-2 flex flex-wrap items-center gap-2">
                  <input
                    type="file"
                    multiple
                    class="file-input file-input-sm w-full sm:w-auto"
                    accept=".trellis,.toml"
                    aria-label={`Files for ${missing.name}`}
                    disabled={busy || uncertain}
                    onchange={(event) => void chooseExtra(missing.name, event)}
                  />
                  <label
                    class={[
                      "btn btn-outline btn-sm",
                      (busy || uncertain) && "btn-disabled",
                    ]}
                    >Choose a folder<input
                      type="file"
                      class="sr-only"
                      webkitdirectory
                      aria-label={`Folder for ${missing.name}`}
                      disabled={busy || uncertain}
                      onchange={(event) =>
                        void chooseExtra(missing.name, event)}
                    /></label
                  >
                </div>
                {#if extraErrors[missing.name]}
                  <p class="mt-2 text-xs text-error" role="alert">
                    {extraErrors[missing.name]}
                  </p>
                {/if}
                {#if extras[missing.name]}
                  <ul
                    class="mt-2 font-mono text-xs text-base-content/70"
                    aria-label={`Files used for ${missing.name}`}
                  >
                    {#each extras[missing.name].names as name (name)}<li>
                        {name}
                      </li>{/each}
                  </ul>
                {/if}
                {#if extras[missing.name] || extraErrors[missing.name]}
                  <button
                    type="button"
                    class="btn btn-ghost btn-xs mt-1"
                    disabled={busy || uncertain}
                    aria-label={`Remove files for ${missing.name}`}
                    onclick={() => removeExtra(missing.name)}>Remove</button
                  >
                {/if}
              </li>
            {/each}
          </ul>
        </section>
      {/if}
      {#if participants.length > 1}
        <label class="trellis-field"
          ><span class="trellis-field-label">Which {kind}</span><select
            class="select w-full"
            bind:value={chosenParticipant}
            required
            disabled={busy || uncertain}
          >
            <option value="" disabled>Choose one</option>
            {#each participants as participant (participant)}
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
      {#if withIdl && declaredSettings.length > 0}
        <fieldset class="trellis-field" disabled={busy || uncertain}>
          <legend class="trellis-field-label mb-1.5">Settings</legend>
          <div class="space-y-3">
            {#each declaredSettings as field (field.name)}
              <SettingInput
                {field}
                id={`new-setting-${field.name}`}
                value={drafts[field.name] ?? ""}
                placeholder={field.default === undefined
                  ? ""
                  : `Default: ${typeof field.default === "string" ? field.default : JSON.stringify(field.default)}`}
                disabled={busy || uncertain}
                missing={field.required && !drafts[field.name]}
                revealed={revealed[field.name] ?? false}
                oninput={(text) => {
                  drafts = { ...drafts, [field.name]: text };
                  if (!text && revealed[field.name])
                    revealed = { ...revealed, [field.name]: false };
                }}
                onreveal={() => {
                  revealed = {
                    ...revealed,
                    [field.name]: !revealed[field.name],
                  };
                }}
              />
            {/each}
          </div>
        </fieldset>
      {:else if !withIdl}
        <div class="trellis-field">
          <span class="trellis-field-label">Settings (optional)</span>
          <FreeSettingRows bind:rows={freeRows} disabled={busy || uncertain} />
          <span class="trellis-field-help"
            >The service checks them against its IDL when it connects.</span
          >
        </div>
      {/if}
      {#if unauthenticated}
        <div class="alert alert-warning block space-y-2" role="alert">
          <p class="font-semibold">A webhook in this build checks nothing.</p>
          <p class="text-sm">
            Anyone who learns its URL can send the service requests.
          </p>
          <label class="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              class="checkbox checkbox-sm"
              bind:checked={unauthenticatedUnderstood}
            />
            I understand this webhook accepts requests from anyone with its URL
          </label>
        </div>
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
