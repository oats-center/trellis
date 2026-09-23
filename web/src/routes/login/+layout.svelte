<script lang="ts">
  import { afterNavigate, beforeNavigate } from "$app/navigation";
  import { browser } from "$app/environment";
  import {
    beginNavigation,
    currentNavigationToken,
    installBrowserTelemetry,
    settleNavigation,
  } from "$lib/browser_telemetry";
  let { children } = $props();

  if (browser) installBrowserTelemetry("portal");

  // Portal keeps shell-only semantics: no authenticated-ready milestone.
  beforeNavigate(() => {
    beginNavigation("portal");
  });
  afterNavigate(() => {
    const token = currentNavigationToken();
    if (token !== undefined) settleNavigation(token);
  });
</script>

<div data-theme="portal" style="display: contents">
  {@render children()}
</div>
