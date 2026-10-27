<script lang="ts">
  import { onMount } from "svelte";
  import type { apis } from "trellis-web-generated";
  import { isErr } from "@oatscenter/result";
  import { ulid } from "ulid";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { traverseAll } from "$lib/console/paging.ts";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";

  type Theme = NonNullable<apis.auth.PortalThemesGetOutput["theme"]>;
  type Palette = Theme["colors"];
  type Logo = NonNullable<Theme["logo"]>;
  type ColorName =
    | "page"
    | "surface"
    | "ink"
    | "muted"
    | "line"
    | "lineStrong"
    | "primary"
    | "primaryHover"
    | "onPrimary"
    | "soft";

  const COLORS: ReadonlyArray<readonly [ColorName, string]> = [
    ["primary", "Buttons and links"],
    ["primaryHover", "Buttons on hover"],
    ["onPrimary", "Text on buttons"],
    ["page", "Page background"],
    ["surface", "Panels"],
    ["ink", "Text"],
    ["muted", "Secondary text"],
    ["line", "Borders"],
    ["lineStrong", "Strong borders"],
    ["soft", "Highlights"],
  ];
  const FORMATS: Record<string, Logo["format"]> = {
    "image/png": "png",
    "image/jpeg": "jpeg",
    "image/webp": "webp",
    "image/svg+xml": "svg",
  };
  const MIME: Record<string, string> = {
    png: "image/png",
    jpeg: "image/jpeg",
    webp: "image/webp",
    svg: "image/svg+xml",
  };

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("portal-theme");
  // "" is the theme every app uses unless it has its own.
  let target = $state("");
  let apps = $state.raw<{ clientId: string; title: string }[]>([]);
  let themed = $state.raw<Set<string>>(new Set());
  let revision = $state(0n);
  let exists = $state(false);
  let loading = $state(true);
  // The form holds the selected target's theme only after it loaded; until
  // then nothing may be saved or removed against that target.
  let loaded = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let name = $state("");
  let logo = $state.raw<Logo | null>(null);
  let colors = $state<Partial<Record<ColorName, string>>>({});
  let darkColors = $state<Partial<Record<ColorName, string>>>({});
  let title = $state("");
  let subtitle = $state("");
  let footer = $state("");
  let stylesheet = $state("");

  const installation = $derived(target === "");
  const editable = $derived(canPerform(authority.authority, "portalThemesPut"));

  function palette(values: Partial<Record<ColorName, string>>): Palette {
    return Object.fromEntries(
      Object.entries(values).filter(([, value]) => value),
    ) as Palette;
  }

  function pick(
    source: Palette | undefined,
  ): Partial<Record<ColorName, string>> {
    const values: Partial<Record<ColorName, string>> = {};
    for (const [key] of COLORS) {
      const value = source?.[key];
      if (typeof value === "string") values[key] = value;
    }
    return values;
  }

  function show(theme: Theme | undefined): void {
    exists = theme !== undefined;
    revision = theme?.revision ?? 0n;
    name = theme?.name ?? "";
    logo = theme?.logo ?? null;
    colors = pick(theme?.colors);
    darkColors = pick(theme?.darkColors);
    title = theme?.copy.signInTitle ?? "";
    subtitle = theme?.copy.signInSubtitle ?? "";
    footer = theme?.copy.footer ?? "";
    stylesheet = theme?.stylesheet ?? "";
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    loading = true;
    loaded = false;
    error = null;
    // Never leave another target's fields and revision in the form.
    show(undefined);
    const result = await trellis
      .portalThemesGet(target ? { clientId: target } : {})
      .take();
    if (!scope.isCurrent(token)) return;
    if (isErr(result)) error = projectConsoleError(result).message;
    else {
      show(result.theme);
      loaded = true;
    }
    loading = false;
  }

  async function loadApps(): Promise<void> {
    const [registered, themes] = await Promise.all([
      traverseAll(async (page) => {
        const response = await trellis.oAuthClientsList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
      traverseAll(async (page) => {
        const response = await trellis.portalThemesList({ page }).orThrow();
        return { items: response.items, cursor: response.page.nextCursor };
      }),
    ]);
    apps = registered.items
      .map((client) => ({ clientId: client.clientId, title: client.title }))
      .sort((left, right) => left.title.localeCompare(right.title));
    themed = new Set(themes.items.flatMap((theme) => theme.clientId ?? []));
  }

  async function chooseLogo(event: Event): Promise<void> {
    const file = (event.currentTarget as HTMLInputElement).files?.[0];
    if (!file) return;
    const format = FORMATS[file.type];
    if (!format) {
      error = "Choose a PNG, JPEG, WebP, or SVG image.";
      return;
    }
    if (file.size > 256 * 1024) {
      error = "Choose an image of 256 KB or less.";
      return;
    }
    const bytes = new Uint8Array(await file.arrayBuffer());
    let binary = "";
    for (const byte of bytes) binary += String.fromCharCode(byte);
    logo = { format, base64: btoa(binary) };
    error = null;
  }

  async function save(event: SubmitEvent): Promise<void> {
    event.preventDefault();
    if (saving || !editable || !loaded) return;
    saving = true;
    error = null;
    notice = null;
    const result = await trellis
      .portalThemesPut({
        ...(target ? { clientId: target } : {}),
        ...(name.trim() ? { name: name.trim() } : {}),
        ...(logo ? { logo } : {}),
        ...(installation && stylesheet.trim() ? { stylesheet } : {}),
        colors: palette(colors),
        darkColors: palette(darkColors),
        copy: {
          ...(title.trim() ? { signInTitle: title.trim() } : {}),
          ...(subtitle.trim() ? { signInSubtitle: subtitle.trim() } : {}),
          ...(footer.trim() ? { footer: footer.trim() } : {}),
        },
        expectedRevision: revision,
        idempotencyKey: ulid(),
      })
      .take();
    if (isErr(result)) error = projectConsoleError(result).message;
    else {
      show(result.theme);
      notice = "Saved. The sign-in page shows it now.";
      void loadApps();
    }
    saving = false;
  }

  async function remove(): Promise<void> {
    if (saving || !exists || !loaded) return;
    saving = true;
    error = null;
    notice = null;
    const result = await trellis
      .portalThemesDelete({
        ...(target ? { clientId: target } : {}),
        expectedRevision: revision,
        idempotencyKey: ulid(),
      })
      .take();
    if (isErr(result)) error = projectConsoleError(result).message;
    else {
      show(undefined);
      notice = installation
        ? "Removed. The sign-in page uses the Trellis look again."
        : "Removed. This app uses the look every app shares.";
      void loadApps();
    }
    saving = false;
  }

  onMount(() => {
    void loadApps();
    return () => scope.dispose();
  });

  $effect(() => {
    void target;
    void load();
  });
</script>

{#snippet swatches(values: Partial<Record<ColorName, string>>)}
  <div class="grid gap-3 sm:grid-cols-2">
    {#each COLORS as [key, label] (key)}
      <div class="flex items-center gap-3">
        <input
          type="color"
          class="h-9 w-12 cursor-pointer rounded border border-base-300"
          aria-label={label}
          value={values[key] ?? "#ffffff"}
          oninput={(event) => (values[key] = event.currentTarget.value)}
        />
        <span class="flex-1 text-sm"
          >{label}
          {#if !values[key]}<span class="text-base-content/50">· default</span
            >{/if}</span
        >
        {#if values[key]}<button
            type="button"
            class="btn btn-ghost btn-xs"
            onclick={() => delete values[key]}>Reset</button
          >{/if}
      </div>
    {/each}
  </div>
{/snippet}

<section class="space-y-5">
  <PageToolbar
    title="Sign-in page"
    description="How the sign-in and approval pages look to people."
  />
  <label class="trellis-field max-w-md"
    ><span class="trellis-field-label">Applies to</span><select
      class="select w-full"
      bind:value={target}
      disabled={saving}
      ><option value="">Every app</option>
      {#each apps as app (app.clientId)}<option value={app.clientId}
          >{app.title}{themed.has(app.clientId) ? " · customized" : ""}</option
        >{/each}</select
    >{#if !installation}<span class="trellis-field-help"
        >Anything left empty uses the look every app shares.</span
      >{/if}</label
  >
  {#if notice}<Notice variant="success">{notice}</Notice>{/if}
  {#if error}<Notice variant="warning">{error}</Notice>{/if}
  {#if loading}<LoadingState
      label="Loading the sign-in page"
    />{:else if !loaded}<button
      class="btn btn-ghost"
      onclick={() => void load()}>Try again</button
    >{:else}
    <form class="trellis-section max-w-2xl space-y-6 p-5" onsubmit={save}>
      <fieldset class="space-y-4" disabled={saving || !editable}>
        <label class="trellis-field"
          ><span class="trellis-field-label">Name</span><input
            class="input w-full"
            maxlength="64"
            placeholder="Trellis"
            aria-describedby="theme-name-help"
            bind:value={name}
          /></label
        ><span id="theme-name-help" class="trellis-field-help -mt-3 block"
          >Shown in the header and the browser tab.</span
        >
        <div class="trellis-field">
          <span class="trellis-field-label">Logo</span>
          <div class="flex items-center gap-4">
            {#if logo}<img
                class="h-10 max-w-40 object-contain"
                alt="Current logo"
                src={`data:${MIME[logo.format]};base64,${logo.base64}`}
              />{/if}
            <input
              type="file"
              class="file-input file-input-sm"
              accept="image/png,image/jpeg,image/webp,image/svg+xml"
              onchange={(event) => void chooseLogo(event)}
            />
            {#if logo}<button
                type="button"
                class="btn btn-ghost btn-sm"
                onclick={() => (logo = null)}>Remove logo</button
              >{/if}
          </div>
        </div>
        <label class="trellis-field"
          ><span class="trellis-field-label">Sign-in heading</span><input
            class="input w-full"
            maxlength="120"
            placeholder="Sign in"
            bind:value={title}
          /></label
        >
        <label class="trellis-field"
          ><span class="trellis-field-label">Sign-in subheading</span><input
            class="input w-full"
            maxlength="280"
            bind:value={subtitle}
          /></label
        >
        <label class="trellis-field"
          ><span class="trellis-field-label">Footer</span><input
            class="input w-full"
            maxlength="500"
            placeholder="Support contact, for example"
            bind:value={footer}
          /></label
        >
        <div class="space-y-3">
          <span class="trellis-field-label">Colours</span>
          {@render swatches(colors)}
        </div>
        <details class="space-y-3">
          <summary class="cursor-pointer text-sm font-medium"
            >Dark mode colours</summary
          >
          <div class="pt-3">{@render swatches(darkColors)}</div>
        </details>
        {#if installation}<details>
            <summary class="cursor-pointer text-sm font-medium"
              >Custom stylesheet</summary
            >
            <textarea
              class="textarea mt-3 h-40 w-full font-mono text-sm"
              maxlength="65536"
              bind:value={stylesheet}
            ></textarea>
          </details>{/if}
      </fieldset>
      {#if editable}<div
          class="flex flex-wrap justify-between gap-2 border-t border-base-300 pt-4"
        >
          <div>
            {#if exists && canPerform(authority.authority, "portalThemesDelete")}<button
                type="button"
                class="btn btn-outline btn-error"
                disabled={saving}
                onclick={() => void remove()}>Remove customization</button
              >{/if}
          </div>
          <button class="btn btn-primary" disabled={saving}>Save</button>
        </div>{/if}
    </form>
  {/if}
</section>
