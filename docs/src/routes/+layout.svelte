<script lang="ts">
  import "../app.css";
  import { onMount, type Snippet } from "svelte";
  import { afterNavigate } from "$app/navigation";
  import { base } from "$app/paths";
  import { page } from "$app/state";
  import TrellisLogo from "$lib/components/TrellisLogo.svelte";
  import {
    designDocsBySection,
    guideSidebarBySection,
    type DocEntry,
    type SidebarItem,
  } from "$lib/docs";
  import { guidesTheme } from "$lib/theme.svelte";

  let { children }: { children: Snippet } = $props();
  let drawerOpen = $state(false);
  type GuideSidebarGroup = Extract<SidebarItem, { kind: "group" }>;

  const pathname = $derived(normalizePath(stripBasePath(page.url.pathname)));
  const guideGroups = guideSidebarBySection();
  const designGroups = designDocsBySection();
  const navItems = [
    { href: "/guides/overview", label: "Overview", description: "What Trellis offers" },
    { href: "/guides/libraries", label: "Libraries", description: "TypeScript and Rust" },
    { href: "/guides/concepts", label: "Concepts", description: "Deeper system model" },
    { href: "/guides", label: "Guides", description: "Task workflows" },
    { href: "/design", label: "Trellis Design Docs", description: "Architecture records" },
    { href: "/api", label: "API Reference", description: "Generated symbols" },
  ];
  const librarySections = [
    { href: "/guides/libraries/typescript", label: "TypeScript libraries" },
    { href: "/guides/libraries/rust", label: "Rust libraries" },
  ];
  const conceptSections = [
    {
      href: "/guides/concepts/architecture",
      label: "Architecture",
      docs: [
        { href: "/guides/concepts/architecture#service-categories", label: "Service categories" },
        { href: "/guides/concepts/architecture#platform-boundary", label: "Platform boundary" },
        { href: "/guides/concepts/architecture#runtime-ownership", label: "Runtime ownership" },
        { href: "/guides/concepts/architecture#public-boundaries", label: "Public boundaries" },
      ],
    },
    {
      href: "/guides/concepts/contracts",
      label: "Contracts",
      docs: [
        { href: "/guides/concepts/contracts#apis-and-participants", label: "APIs and participants" },
        { href: "/guides/concepts/contracts#identity-and-evidence", label: "Identity and evidence" },
        {
          href: "/guides/concepts/contracts#generated-packages",
          label: "Generated packages",
        },
      ],
    },
    {
      href: "/guides/concepts/deployment-authority",
      label: "Deployment Authority",
      docs: [
        { href: "/guides/concepts/deployment-authority#installed-participant-revisions", label: "Installed revisions" },
        { href: "/guides/concepts/deployment-authority#identity-grants", label: "Identity grants" },
        { href: "/guides/concepts/deployment-authority#deployments-and-resources", label: "Resources" },
        { href: "/guides/concepts/deployment-authority#administration", label: "Administration" },
        { href: "/guides/concepts/deployment-authority#runtime-issuance", label: "Runtime issuance" },
      ],
    },
    {
      href: "/guides/concepts/communication",
      label: "Communication",
      docs: [
        { href: "/guides/concepts/communication#rpcs-requestreply", label: "RPCs" },
        {
          href: "/guides/concepts/communication#operations-async-workflows",
          label: "Operations",
        },
        {
          href: "/guides/concepts/communication#events-jetstream-pubsub",
          label: "Events",
        },
        {
          href: "/guides/concepts/communication#feeds-authorized-live-views",
          label: "Feeds",
        },
        {
          href: "/guides/concepts/communication#cross-api-dependencies",
          label: "Dependencies",
        },
        {
          href: "/guides/concepts/communication#surface-availability",
          label: "Surface availability",
        },
      ],
    },
    {
      href: "/guides/concepts/authentication-and-authorization",
      label: "Authentication and authorization",
      docs: [
        {
          href: "/guides/concepts/authentication-and-authorization#principals",
          label: "Principals",
        },
        {
          href: "/guides/concepts/authentication-and-authorization#session-keys",
          label: "Session keys",
        },
        {
          href: "/guides/concepts/authentication-and-authorization#browser-login-and-the-portal",
          label: "Browser login and portal",
        },
        {
          href: "/guides/concepts/authentication-and-authorization#capabilities",
          label: "Capabilities",
        },
      ],
    },
    {
      href: "/guides/concepts/resources",
      label: "Resources",
      docs: [
        { href: "/guides/concepts/resources#state", label: "State" },
        { href: "/guides/concepts/resources#kv", label: "KV" },
        { href: "/guides/concepts/resources#store", label: "Store" },
        { href: "/guides/concepts/resources#consumers-and-jobs", label: "Consumers and jobs" },
      ],
    },
    {
      href: "/guides/concepts/files-and-transfers",
      label: "Files and transfers",
      docs: [
        { href: "/guides/concepts/files-and-transfers#files-vs-store-resources", label: "Files vs store resources" },
        { href: "/guides/concepts/files-and-transfers#metadata-and-control-rpcs", label: "Metadata and control RPCs" },
        { href: "/guides/concepts/files-and-transfers#send-transfers", label: "Send transfers" },
        { href: "/guides/concepts/files-and-transfers#receive-transfers", label: "Receive transfers" },
        { href: "/guides/concepts/files-and-transfers#operations-and-transfers", label: "Operations and transfers" },
        { href: "/guides/concepts/files-and-transfers#store-backing", label: "Store backing" },
      ],
    },
    {
      href: "/guides/concepts/type-system-and-errors",
      label: "Type system and errors",
      docs: [
        { href: "/guides/concepts/type-system-and-errors#schemas", label: "Schemas" },
        { href: "/guides/concepts/type-system-and-errors#schema-validation", label: "Schema validation" },
        { href: "/guides/concepts/type-system-and-errors#declared-errors", label: "Declared errors" },
        { href: "/guides/concepts/type-system-and-errors#result-values", label: "Result values" },
        { href: "/guides/concepts/type-system-and-errors#pagination-shapes", label: "Pagination shapes" },
        { href: "/guides/concepts/type-system-and-errors#storage-identity", label: "Storage identity" },
      ],
    },
    {
      href: "/guides/concepts/devices-and-activation",
      label: "Devices and activation",
      docs: [
        { href: "/guides/concepts/devices-and-activation#preregistered-devices", label: "Preregistered devices" },
        { href: "/guides/concepts/devices-and-activation#device-identity", label: "Device identity" },
        { href: "/guides/concepts/devices-and-activation#device-deployments", label: "Device deployments" },
        { href: "/guides/concepts/devices-and-activation#activation-portals", label: "Activation portals" },
        { href: "/guides/concepts/devices-and-activation#review-policy", label: "Review policy" },
        { href: "/guides/concepts/devices-and-activation#online-credentials", label: "Online credentials" },
      ],
    },
    { href: "/guides/concepts/jobs", label: "Jobs", docs: [] },
  ];
  const pageTitle = $derived(
    navItems.find((item) => isNavItemActive(item.href))?.label ?? "Documentation",
  );

  function normalizePath(path: string) {
    return path.length > 1 && path.endsWith("/") ? path.slice(0, -1) : path;
  }

  function stripBasePath(pathname: string) {
    if (!base) {
      return pathname;
    }

    if (pathname === base) {
      return "/";
    }

    return pathname.startsWith(`${base}/`) ? pathname.slice(base.length) : pathname;
  }

  function resolveDocHref(href: string) {
    if (!base) {
      return href;
    }

    return href === "/" ? `${base}/` : `${base}${href}`;
  }

  function isActive(href: string) {
    return href === "/" ? pathname === "/" : pathname === href || pathname.startsWith(`${href}/`);
  }

  function isNavItemActive(href: string) {
    if (href === "/guides") {
      return isActive(href) && !isActive("/guides/concepts") &&
        !isActive("/guides/overview") && !isActive("/guides/libraries");
    }

    return isActive(href);
  }

  function isGuidesActive() {
    return isNavItemActive("/guides");
  }

  function isConceptsActive() {
    return isActive("/guides/concepts");
  }

  function isConceptSectionActive(section: { href: string }) {
    return pathname === section.href;
  }

  function isDesignActive() {
    return isActive("/design");
  }

  function isGuideGroupActive(item: GuideSidebarGroup) {
    return item.parent.href === pathname || item.docs.some((doc) => doc.href === pathname);
  }

  function designSectionParent(section: { docs: DocEntry[] }) {
    return section.docs.find((doc) => {
      const relative = doc.href.replace(/^\/design\/?/, "");
      return doc.href === "/design" || relative.length > 0 && !relative.includes("/");
    }) ?? section.docs[0] ?? null;
  }

  function designSectionDocs(section: { docs: DocEntry[] }) {
    const parent = designSectionParent(section);
    return parent ? section.docs.filter((doc) => doc.href !== parent.href) : section.docs;
  }

  function isDesignSectionActive(section: { docs: DocEntry[] }) {
    return section.docs.some((doc) =>
      doc.href === "/design"
        ? pathname === doc.href
        : doc.href === pathname || pathname.startsWith(`${doc.href}/`)
    );
  }

  function displayDesignSection(section: string) {
    return section.replace(/^Design:\s*/, "").replace(/^Design Overview$/, "Overview");
  }

  function closeDrawer() {
    drawerOpen = false;
  }

  afterNavigate(() => {
    closeDrawer();
  });

  onMount(() => {
    guidesTheme.init();
  });
</script>

<a class="skip-link btn btn-sm btn-primary" href="#trellis-docs-main">Skip to main content</a>

<div class="drawer min-h-screen bg-base-200 text-base-content lg:drawer-open">
  <input id="trellis-docs-nav" type="checkbox" class="drawer-toggle" bind:checked={drawerOpen} />

  <div class="drawer-content flex min-w-0 flex-col">
    <header class="navbar trellis-topbar sticky top-0 z-30 h-16 min-h-16 border-b border-base-300 bg-base-100/95 px-4 lg:px-7">
      <div class="navbar-start gap-3">
        <button
          type="button"
          class="btn btn-square btn-ghost lg:hidden"
          aria-label="Toggle navigation"
          onclick={() => { drawerOpen = !drawerOpen; }}
        >
          <svg aria-hidden="true" class="size-5" viewBox="0 0 20 20" fill="none">
            <path d="M3 5h14M3 10h14M3 15h14" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" />
          </svg>
        </button>
        <div>
          <p class="docs-section-label">Trellis documentation</p>
          <p class="text-sm font-semibold text-base-content">{pageTitle}</p>
        </div>
      </div>
      <div class="navbar-end gap-2 md:flex">
        <label class="swap swap-rotate btn btn-ghost btn-square btn-sm">
          <input type="checkbox" checked={guidesTheme.darkMode} onchange={() => guidesTheme.toggle()} aria-label="Toggle dark mode" />
          <svg aria-hidden="true" class="swap-off size-5" viewBox="0 0 20 20" fill="none">
            <path d="M10 3v2M10 15v2M3 10h2M15 10h2M5.05 5.05l1.42 1.42M13.53 13.53l1.42 1.42M14.95 5.05l-1.42 1.42M6.47 13.53l-1.42 1.42" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
            <circle cx="10" cy="10" r="3.2" stroke="currentColor" stroke-width="1.6" />
          </svg>
          <svg aria-hidden="true" class="swap-on size-5" viewBox="0 0 20 20" fill="none">
            <path d="M15.4 12.2A5.8 5.8 0 0 1 7.8 4.6 6.5 6.5 0 1 0 15.4 12.2Z" stroke="currentColor" stroke-width="1.6" stroke-linejoin="round" />
          </svg>
        </label>
      </div>
    </header>

    <main id="trellis-docs-main" tabindex="-1" class="mx-auto w-full max-w-[1320px] flex-1 px-4 py-7 outline-none lg:px-8">
      {@render children()}
    </main>
  </div>

  <div class="drawer-side z-40">
    <label for="trellis-docs-nav" class="drawer-overlay" aria-hidden="true"></label>

    <aside class="trellis-sidebar flex min-h-full w-72 flex-col">
      <div class="flex h-[76px] items-center gap-3 px-6">
        <a href={resolveDocHref("/")} aria-label="Trellis documentation home">
          <TrellisLogo subtitle="Docs" markClass="trellis-logo-orange" titleClass="text-white" subtitleClass="text-slate-400" />
        </a>
        <button type="button" class="btn btn-square btn-ghost btn-sm ml-auto lg:hidden" aria-label="Close navigation" onclick={closeDrawer}>
          <svg aria-hidden="true" class="size-4" viewBox="0 0 20 20" fill="none">
            <path d="m5 5 10 10M15 5 5 15" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" />
          </svg>
        </button>
      </div>

      <nav class="flex-1 overflow-y-auto px-3 pt-3" aria-label="Primary">
        <p class="mb-2 px-3 text-[11px] font-semibold uppercase tracking-[0.08em] text-slate-500">Documentation</p>
        <div class="trellis-sidebar-nav">
          {#each navItems as item (item.href)}
            <div class="trellis-sidebar-nav-item">
              <a href={resolveDocHref(item.href)} class={["trellis-sidebar-main-link", { active: isNavItemActive(item.href) }]} aria-current={isNavItemActive(item.href) ? "page" : undefined} onclick={closeDrawer}>
                <span>
                  <span class="block">{item.label}</span>
                  <span class="block text-[11px] text-inherit opacity-55">{item.description}</span>
                </span>
              </a>

              {#if item.href === "/guides/concepts" && isConceptsActive()}
                <div class="trellis-sidebar-nested" aria-label="Trellis concepts navigation">
                  {#each conceptSections as section (section.href)}
                    <a
                      class={[
                        "trellis-sidebar-section-label",
                        "trellis-sidebar-section-link",
                        isConceptSectionActive(section) && "active",
                      ]}
                      href={resolveDocHref(section.href)}
                      aria-current={isConceptSectionActive(section) ? "page" : undefined}
                      onclick={closeDrawer}
                    >
                      {section.label}
                    </a>

                    {#if isConceptSectionActive(section) && section.docs.length > 0}
                      <div class="trellis-sidebar-nested-children">
                        {#each section.docs as doc (doc.href)}
                          <a
                            class={[
                              "trellis-sidebar-nested-link",
                              "trellis-sidebar-child-link",
                            ]}
                            href={resolveDocHref(doc.href)}
                            onclick={closeDrawer}
                          >
                            {doc.label}
                          </a>
                        {/each}
                      </div>
                    {/if}
                  {/each}
                </div>
              {/if}

              {#if item.href === "/guides/libraries" && isNavItemActive(item.href)}
                <div class="trellis-sidebar-nested" aria-label="Libraries navigation">
                  {#each librarySections as section (section.href)}
                    <a
                      class={[
                        "trellis-sidebar-nested-link",
                        pathname === section.href && "active",
                      ]}
                      href={resolveDocHref(section.href)}
                      aria-current={pathname === section.href ? "page" : undefined}
                      onclick={closeDrawer}
                    >
                      {section.label}
                    </a>
                  {/each}
                </div>
              {/if}

              {#if item.href === "/guides" && isGuidesActive()}
                <div class="trellis-sidebar-nested" aria-label="Guides navigation">
                  {#each guideGroups as group (group.section)}
                    <p class="trellis-sidebar-section-label">{group.section}</p>

                    {#each group.items as guideItem (guideItem.kind === "doc" ? guideItem.doc.href : guideItem.label)}
                      {#if guideItem.kind === "group"}
                        <a
                          class={[
                            "trellis-sidebar-nested-link",
                            isGuideGroupActive(guideItem) && "active",
                          ]}
                          href={resolveDocHref(guideItem.parent.href)}
                          aria-current={pathname === guideItem.parent.href ? "page" : undefined}
                          onclick={closeDrawer}
                        >
                          {guideItem.label}
                        </a>

                        {#if isGuideGroupActive(guideItem) && guideItem.docs.length > 0}
                          <div class="trellis-sidebar-nested-children">
                            {#each guideItem.docs as doc (doc.href)}
                              <a
                                class={[
                                  "trellis-sidebar-nested-link",
                                  "trellis-sidebar-child-link",
                                  pathname === doc.href && "active",
                                ]}
                                href={resolveDocHref(doc.href)}
                                aria-current={pathname === doc.href ? "page" : undefined}
                                onclick={closeDrawer}
                              >
                                {doc.sidebarLabel ?? doc.title}
                              </a>
                            {/each}
                          </div>
                        {/if}
                      {:else}
                        <a
                          class={[
                            "trellis-sidebar-nested-link",
                            pathname === guideItem.doc.href && "active",
                          ]}
                          href={resolveDocHref(guideItem.doc.href)}
                          aria-current={pathname === guideItem.doc.href ? "page" : undefined}
                          onclick={closeDrawer}
                        >
                          {guideItem.doc.title}
                        </a>
                      {/if}
                    {/each}
                  {/each}
                </div>
              {/if}

              {#if item.href === "/design" && isDesignActive()}
                <div class="trellis-sidebar-nested" aria-label="Design navigation">
                  {#each designGroups as group (group.section)}
                    {@const parent = designSectionParent(group)}
                    {@const docs = designSectionDocs(group)}
                    {@const groupActive = isDesignSectionActive(group)}
                    <p class="trellis-sidebar-section-label">{displayDesignSection(group.section)}</p>

                    {#if parent}
                      <a
                        class={[
                          "trellis-sidebar-nested-link",
                          groupActive && "active",
                        ]}
                        href={resolveDocHref(parent.href)}
                        aria-current={pathname === parent.href ? "page" : undefined}
                        onclick={closeDrawer}
                      >
                        {parent.sidebarLabel ?? parent.title}
                      </a>
                    {/if}

                    {#if groupActive && docs.length > 0}
                      <div class="trellis-sidebar-nested-children">
                        {#each docs as doc (doc.href)}
                          <a
                            class={[
                              "trellis-sidebar-nested-link",
                              "trellis-sidebar-child-link",
                              pathname === doc.href && "active",
                            ]}
                            href={resolveDocHref(doc.href)}
                            aria-current={pathname === doc.href ? "page" : undefined}
                            onclick={closeDrawer}
                          >
                            {doc.sidebarLabel ?? doc.title}
                          </a>
                        {/each}
                      </div>
                    {/if}
                  {/each}
                </div>
              {/if}
            </div>
          {/each}
        </div>
      </nav>
    </aside>
  </div>
</div>
