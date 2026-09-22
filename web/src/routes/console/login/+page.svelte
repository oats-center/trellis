<script lang="ts">
  import { browser } from "$app/environment";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { onMount } from "svelte";
  import { APP_CONFIG, getCanonicalLoopbackRedirectUrl } from "$lib/config";
  import { getConsoleRedirectTarget } from "$lib/auth";
  import Notice from "$lib/components/Notice.svelte";

  onMount(() => {
    if (!browser) return;
    const canonicalRedirect = getCanonicalLoopbackRedirectUrl();
    if (canonicalRedirect) {
      window.location.replace(canonicalRedirect);
      return;
    }
    if (APP_CONFIG.authUrl) void goto(getConsoleRedirectTarget(page.url));
  });
</script>

<svelte:head><title>Sign In · Trellis</title></svelte:head>

{#if !APP_CONFIG.authUrl}
  <div class="flex min-h-screen flex-col items-center justify-center gap-4 bg-base-200 px-4 text-center">
    <h1 class="text-xl font-semibold">Trellis is not configured</h1>
    <Notice variant="error" class="max-w-md text-sm">
      Set the Console Trellis endpoint in runtime configuration.
    </Notice>
  </div>
{/if}
