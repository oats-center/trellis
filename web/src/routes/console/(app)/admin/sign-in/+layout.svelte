<script lang="ts">
  import type { Snippet } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { resolve } from "$lib/console_paths";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import PageToolbar from "$lib/components/PageToolbar.svelte";

  let { children }: { children: Snippet } = $props();

  const authority = getConsoleAuthority();
  const methodsHref = resolve("/admin/sign-in");
  const appearanceHref = resolve("/admin/sign-in/appearance");
  // Methods need "Manage this Trellis" or "Manage users"; appearance needs
  // the Portal themes. Each tab shows only to those who can use it.
  const tabs = $derived(
    [
      {
        href: methodsHref,
        label: "Methods",
        allowed:
          canPerform(authority.authority, "signInSettingsGet") ||
          canPerform(authority.authority, "oidcProvidersList"),
      },
      {
        href: appearanceHref,
        label: "Appearance",
        allowed: canPerform(authority.authority, "portalThemesGet"),
      },
    ].filter((tab) => tab.allowed),
  );
  const current = $derived(page.url.pathname.replace(/\/$/, ""));

  // Someone who may only change the look lands on it from the nav item.
  $effect(() => {
    if (
      authority.authority !== null &&
      current === methodsHref &&
      tabs.length === 1 &&
      tabs[0].href === appearanceHref
    )
      void goto(appearanceHref, { replaceState: true });
  });
</script>

<section>
  <PageToolbar
    title="Sign-in"
    description="How users sign in to this Trellis, and how the sign-in page looks."
  />
  {#if tabs.length > 1}
    <nav class="trellis-segment mb-6 w-fit" aria-label="Sign-in">
      {#each tabs as tab (tab.href)}
        <a
          href={tab.href}
          aria-current={current === tab.href ? "page" : undefined}
          >{tab.label}</a
        >
      {/each}
    </nav>
  {/if}
  {@render children()}
</section>
