<script lang="ts">
  import type { AuthorityProfile } from "../console/authority.ts";
  import { afterNavigate } from "$app/navigation";
  import { base, resolve } from "$lib/console_paths";
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
  import NotAllowed from "./NotAllowed.svelte";
  import TrellisLogo from "./TrellisLogo.svelte";
  import ToastViewport from "./ToastViewport.svelte";

  type Props = {
    children: Snippet;
    profile: AuthorityProfile | null;
    authority: Authority | null;
    navSections: NavSection[];
    connectionStatus: ConnectionStatus["phase"];
    authFailure: string | null;
    accountNotAllowed: boolean;
    onRetryAuthority?: () => void;
    onSignOut: () => Promise<void> | void;
  };

  let {
    children,
    profile,
    authority,
    navSections,
    connectionStatus,
    authFailure,
    accountNotAllowed,
    onRetryAuthority,
    onSignOut,
  }: Props = $props();

  let drawerOpen = $state(false);
  let paletteOpen = $state(false);

  const commandIndex = $derived(buildCommandIndex(navSections));
  const paletteShortcut = $derived(
    typeof navigator !== "undefined" &&
      /Mac|iPhone|iPad/.test(navigator.platform ?? navigator.userAgent)
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

  function toRoutePath(pathname: string): string {
    if (base && pathname === base) {
      return "/";
    }

    if (base && pathname.startsWith(`${base}/`)) {
      return pathname.slice(base.length);
    }

    return pathname;
  }

  // Detail and action pages keep their section highlighted.
  function isActive(href: string): boolean {
    if (href === "/admin") return routePath === href;
    return routePath === href || routePath.startsWith(`${href}/`);
  }

  function closeDrawer(): void {
    drawerOpen = false;
  }

  afterNavigate(() => {
    closeDrawer();
  });
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<svelte:head>
  <title>{pageTitle} · Trellis</title>
</svelte:head>

<a class="skip-link btn btn-sm btn-primary" href="#trellis-main"
  >Skip to main content</a
>

<div class="drawer min-h-screen bg-base-200 lg:drawer-open">
  <input
    id="trellis-nav"
    type="checkbox"
    class="drawer-toggle"
    bind:checked={drawerOpen}
  />

  <div class="drawer-content flex min-w-0 flex-col">
    <header
      class="trellis-topbar sticky top-0 z-30 flex h-16 items-center gap-3 bg-base-200 px-4 lg:px-6"
    >
      <button
        type="button"
        class="btn btn-square btn-ghost btn-sm lg:hidden"
        aria-label="Open navigation"
        onclick={() => {
          drawerOpen = !drawerOpen;
        }}
      >
        <Icon name="menu" size={20} />
      </button>
      <button
        type="button"
        class="flex h-10 w-full max-w-lg items-center gap-3 rounded-field border border-base-300 bg-base-100 px-3.5 text-left text-sm text-base-content/55 transition-colors hover:border-base-content/25"
        aria-label="Search Trellis"
        onclick={() => {
          paletteOpen = true;
        }}
      >
        <Icon name="search" size={17} />
        <span class="flex-1">Search Trellis…</span>
        <kbd class="kbd kbd-xs hidden md:inline-flex">{paletteShortcut}</kbd>
      </button>
      <div class="ml-auto flex items-center gap-2 sm:gap-3">
        <label
          class="swap swap-rotate btn btn-ghost btn-square"
          title="Toggle dark mode"
        >
          <input
            type="checkbox"
            checked={consoleTheme.darkMode}
            onchange={() => consoleTheme.toggle()}
            aria-label="Toggle dark mode"
          />
          <Icon name="sun" size={20} class="swap-off" />
          <Icon name="moon" size={20} class="swap-on" />
        </label>
        {#if profile}
          <ActionMenu
            buttonBaseClass="btn btn-ghost gap-1.5 rounded-full px-1.5"
            widthClass="w-64"
            ariaLabel="Open user menu"
          >
            {#snippet summary()}
              <span
                class="grid h-9 w-9 place-items-center rounded-full bg-primary text-sm font-medium text-primary-content"
                >{getInitials(profile)}</span
              >
              <Icon name="chevronDown" size={16} class="opacity-60" />
            {/snippet}
            <li class="menu-title px-2 py-2 normal-case">
              <div class="min-w-0">
                <p class="truncate text-sm font-medium text-base-content">
                  {profile.displayName}
                </p>
                <p class="text-xs text-base-content/60">
                  {getRoleLabel(authority)}
                </p>
              </div>
            </li>
            <li>
              <a href={resolve("/profile")}
                ><Icon name="user" size={16} />Account</a
              >
            </li>
            <li>
              <button type="button" onclick={onSignOut}
                ><Icon name="logOut" size={16} />Sign out</button
              >
            </li>
          </ActionMenu>
        {/if}
      </div>
    </header>

    <div class="trellis-workspace flex flex-1 flex-col">
      <main
        id="trellis-main"
        tabindex="-1"
        class="mx-auto w-full max-w-[1500px] flex-1 px-4 py-6 outline-none lg:px-8 lg:py-7"
      >
        {#if authFailure}
          <Notice variant="error" class="mb-4">
            {authFailure}
            {#if onRetryAuthority}
              <button
                class="btn btn-ghost btn-xs ml-2"
                type="button"
                onclick={onRetryAuthority}>Retry</button
              >
            {/if}
          </Notice>
        {/if}

        {#if accountNotAllowed}
          <NotAllowed what="your account" class="mx-auto mt-10 max-w-xl" />
        {:else}
          {@render children()}
        {/if}
      </main>
    </div>

    <ToastViewport />
  </div>

  <div class="drawer-side z-40">
    <label for="trellis-nav" class="drawer-overlay" aria-hidden="true"></label>

    <aside class="trellis-sidebar flex min-h-full w-[244px] flex-col px-3 pb-4">
      <div class="flex h-16 items-center px-3">
        <a href={resolve("/admin")} aria-label="Trellis home">
          <TrellisLogo size={26} />
        </a>
        <button
          type="button"
          class="btn btn-square btn-ghost btn-sm ml-auto lg:hidden"
          aria-label="Close navigation"
          onclick={closeDrawer}
        >
          <Icon name="close" size={18} />
        </button>
      </div>

      <nav class="flex flex-1 flex-col pt-3" aria-label="Primary">
        {#each navSections as section, index (section.title)}
          <div
            class={[
              index > 0 && "mt-4 border-t border-base-300 pt-4",
              section.title === "Account" && "mt-auto",
            ]}
          >
            {#if section.title && section.title !== "Account"}
              <p class="trellis-sidebar-label mb-1.5">{section.title}</p>
            {/if}
            <ul class="menu w-full gap-0.5 p-0">
              {#each section.items as item (item.href)}
                <li>
                  <a
                    href={resolve(item.href)}
                    class={{ active: isActive(item.href) }}
                    aria-current={isActive(item.href) ? "page" : undefined}
                    onclick={closeDrawer}
                    ><Icon name={item.icon} size={19} />{item.label}</a
                  >
                </li>
              {/each}
            </ul>
          </div>
        {/each}
      </nav>
    </aside>
  </div>
</div>

<CommandPalette commands={commandIndex} bind:open={paletteOpen} />

{#if connectionStatus !== "connected"}
  <div
    class="fixed inset-0 z-50 grid place-items-center bg-base-100/85 p-6 backdrop-blur-sm"
    role="alert"
    aria-live="assertive"
  >
    <div class="max-w-sm text-center">
      <span class="loading loading-spinner loading-lg text-base-content/60"
      ></span>
      <p class="mt-4 text-lg font-semibold">
        {connectionStatus === "reconnecting"
          ? "Reconnecting to Trellis…"
          : "Can't reach Trellis"}
      </p>
      <p class="mt-1 text-sm text-base-content/60">
        This page picks up where you left off once the connection is back.
      </p>
    </div>
  </div>
{/if}
