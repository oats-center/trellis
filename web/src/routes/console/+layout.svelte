<script lang="ts">
  import { afterNavigate, beforeNavigate } from "$app/navigation";
  import { browser } from "$app/environment";
  import { onMount } from "svelte";
  import {
    beginNavigation,
    currentNavigationToken,
    installBrowserTelemetry,
    settleNavigation,
  } from "$lib/browser_telemetry";
  import { consoleTheme } from "$lib/theme.svelte";
  let { children } = $props();

  if (browser) installBrowserTelemetry("console");
  onMount(() => {
    consoleTheme.init();
  });

  // The router's before boundary starts each navigation's token; its after
  // boundary captures the shell milestone. Using after as the start would
  // report near-zero durations and omit the navigation elapsed time.
  beforeNavigate(() => {
    beginNavigation("console");
  });
  afterNavigate(() => {
    const token = currentNavigationToken();
    if (token !== undefined) settleNavigation(token);
  });
</script>

{@render children()}
