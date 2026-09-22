<script lang="ts">
  import { type apis } from "trellis-web-generated";
  import { afterNavigate } from "$app/navigation";
  import { base } from "$lib/console_paths";
  import { page } from "$app/state";
  import type { Snippet } from "svelte";
  import type { ConnectionStatus } from "../trellis";
  import { consoleTheme } from "../theme.svelte";
  import {
    getInitials,
    getPageTitle,
    getRoleLabel,
    type Authority,
    type NavSection,
  } from "../control-panel.ts";
  import ActionMenu from "./ActionMenu.svelte";
  import CommandPalette from "./CommandPalette.svelte";
  import { buildCommandIndex } from "../commands.ts";
  import Icon from "./Icon.svelte";
  import Notice from "./Notice.svelte";
  import StatusBadge from "./StatusBadge.svelte";
  import TrellisLogo from "./TrellisLogo.svelte";
  import ToastViewport from "./ToastViewport.svelte";

  type Props = {
    children: Snippet;
    profile: apis.auth.SessionsMeOutput["user"] | null;
    authority: Authority | null;
    profileLoaded: boolean;
    navSections: NavSection[];
    connectionStatus: ConnectionStatus["phase"];
    authFailure: string | null;
    onRetryAuthority?: () => void;
    onSignOut: () => Promise<void> | void;
  };

  let {
    children,
    profile,
    authority,
    profileLoaded,
    navSections,
    connectionStatus,
    authFailure,
    onRetryAuthority,
    onSignOut,
  }: Props = $props();

  let drawerOpen = $state(false);
  let paletteOpen = $state(false);

  const commandIndex = $derived(buildCommandIndex(navSections));
  const paletteShortcut = $derived(
    typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent)
      ? "⌘K"
      : "Ctrl K",
  );

  function handleWindowKeydown(event: KeyboardEvent) {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      paletteOpen = !paletteOpen;
    }
  }

  const routePath = $derived(toRoutePath(page.url.pathname));
  const pageTitle = $derived(getPageTitle(routePath));
  const connectionLabel = $derived(
    connectionStatus === "connected" ? "Connected" :
      connectionStatus === "reconnecting" ? "Reconnecting" : "Offline"
  );
  const connectionVariant = $derived(
    connectionStatus === "connected" ? "healthy" :
      connectionStatus === "reconnecting" ? "degraded" : "unhealthy"
  );

  function toRoutePath(pathname: string): string {
    if (base && pathname === base) {
      return "/";
    }

    if (base && pathname.startsWith(`${base}/`)) {
      return pathname.slice(base.length);
    }

    return pathname;
  }

  function closeDrawer(): void {
    drawerOpen = false;
  }

  function resolveAppPath(path: string): string {
    if (base && (path === base || path.startsWith(`${base}/`))) {
      return path;
    }

    return `${base}${path.startsWith("/") ? path : `/${path}`}`;
  }

  afterNavigate(() => {
    closeDrawer();
  });
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<svelte:head>
  <title>{pageTitle} · Trellis</title>
</svelte:head>

<a class="skip-link btn btn-sm btn-primary" href="#trellis-main">Skip to main content</a>

<div class="drawer min-h-screen bg-base-200 lg:drawer-open">
  <input id="trellis-nav" type="checkbox" class="drawer-toggle" bind:checked={drawerOpen} />

  <div class="drawer-content flex min-w-0 flex-col">
    <header class="navbar trellis-topbar sticky top-0 z-30 h-16 min-h-16 border-b border-base-300 bg-base-100 px-4 lg:px-7">
      <div class="navbar-start gap-3">
        <button
          type="button"
          class="btn btn-square btn-ghost"
          aria-label="Toggle navigation"
          onclick={() => { drawerOpen = !drawerOpen; }}
        >
          <Icon name="menu" size={20} />
        </button>
        <button
          type="button"
          class="btn btn-ghost btn-sm h-9 gap-2 border border-base-300 bg-base-200/60 px-3 font-normal text-base-content/70"
          aria-label="Open command palette"
          onclick={() => { paletteOpen = true; }}
        >
          <Icon name="search" size={15} class="opacity-60" />
          <span class="hidden md:inline">Search pages and views</span>
          <kbd class="kbd kbd-xs hidden md:inline-flex">{paletteShortcut}</kbd>
        </button>
      </div>
      <div class="navbar-end gap-2 sm:gap-3">
        <StatusBadge label={`${connectionLabel}: Trellis`} status={connectionVariant} class="hidden sm:inline-flex px-3" />
        <div class="divider divider-horizontal mx-0 hidden lg:flex"></div>
        <label class="swap swap-rotate btn btn-ghost btn-square btn-sm">
          <input type="checkbox" checked={consoleTheme.darkMode} onchange={() => consoleTheme.toggle()} aria-label="Toggle dark mode" />
          <Icon name="sun" size={20} class="swap-off" />
          <Icon name="moon" size={20} class="swap-on" />
        </label>
        {#if profile}
          <ActionMenu buttonBaseClass="btn btn-ghost gap-2 rounded-full pr-2" widthClass="w-64" ariaLabel="Open user menu">
            {#snippet summary()}
              {#if profile.image}
                <div class="avatar">
                  <div class="w-8 rounded-full">
                    <img src={profile.image} alt={profile.name} />
                  </div>
                </div>
              {:else}
                <div class="avatar avatar-placeholder">
                  <div class="w-8 rounded-full bg-neutral text-neutral-content">
                    <span class="text-xs">{getInitials(profile)}</span>
                  </div>
                </div>
              {/if}
              <Icon name="chevronDown" size={16} class="opacity-60" />
            {/snippet}
            <li class="menu-title px-2 py-2 normal-case">
              <div class="min-w-0">
                <p class="truncate text-sm font-medium">{profile.name}</p>
                <p class="text-xs text-base-content/60">{getRoleLabel(authority)}</p>
              </div>
            </li>
            <li><a class="btn btn-ghost btn-sm justify-start" href={resolveAppPath("/profile")}>Account</a></li>
            <li><button type="button" class="btn btn-ghost btn-sm justify-start" onclick={onSignOut}>Sign out</button></li>
          </ActionMenu>
        {/if}
      </div>
    </header>

    <main
      id="trellis-main"
      tabindex="-1"
      class="mx-auto w-full max-w-[1500px] flex-1 px-4 py-7 outline-none lg:px-8"
    >
      {#if authFailure}
        <Notice variant="error" class="mb-4">
          {authFailure}
          {#if onRetryAuthority}
            <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={onRetryAuthority}>Retry</button>
          {/if}
        </Notice>
      {/if}

      {@render children()}
    </main>

    <ToastViewport />
  </div>

  <div class="drawer-side z-40">
    <label for="trellis-nav" class="drawer-overlay" aria-hidden="true"></label>

    <aside class="trellis-sidebar flex min-h-full w-[251px] flex-col">
      <div class="flex h-[76px] items-center gap-3 px-6">
        <a href={resolveAppPath("/admin")} aria-label="Trellis Console home">
          <TrellisLogo subtitle="Console" markClass="trellis-logo-orange" titleClass="text-white" subtitleClass="text-slate-400" />
        </a>
        <button type="button" class="btn btn-square btn-ghost btn-sm ml-auto lg:hidden" aria-label="Close navigation" onclick={closeDrawer}>
          <Icon name="menu" size={18} />
        </button>
      </div>

      <div class="hidden border-b border-neutral-content/10"></div>

      <nav class="flex-1 space-y-8 overflow-y-auto px-3 pt-3" aria-label="Primary">
        {#each navSections as section (section.title)}
          <div>
            <p class="mb-2 px-3 text-[11px] font-semibold uppercase tracking-[0.08em] text-slate-500">{section.title}</p>
            <ul class="menu gap-1 p-0">
              {#each section.items as item (item.href)}
                <li>
                  <a href={resolveAppPath(item.href)} class={{ active: routePath === item.href }} aria-current={routePath === item.href ? "page" : undefined} onclick={closeDrawer}><Icon name={item.icon} size={16} />{item.label}</a>
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </nav>

      <div class="m-3 rounded-box border border-white/10 bg-white/5 p-4 text-sm">
        <div class="mb-3 flex items-center gap-2 text-slate-100">
          <span class={["h-2.5 w-2.5 rounded-full", connectionStatus === "connected" ? "bg-success" : connectionStatus === "reconnecting" ? "bg-warning" : "bg-error"]}></span>
          {connectionLabel}: Trellis
        </div>
        <div class="text-slate-400">Trellis Runtime</div>
      </div>
    </aside>
  </div>
</div>

<CommandPalette commands={commandIndex} bind:open={paletteOpen} />
