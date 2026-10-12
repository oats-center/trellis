<script lang="ts">
  import { onMount } from "svelte";
  import { TrellisClient } from "@oatscenter/trellis";
  import { APP_CONFIG, consoleAuthOptions } from "$lib/config";
  import { consoleReturnKey, getConsoleRedirectTarget } from "$lib/auth";
  import { trellisApp } from "$lib/trellis-context.svelte";
  import PortalBrand from "$lib/components/PortalBrand.svelte";

  let failed = $state(false);

  onMount(() => {
    const abort = new AbortController();
    let redirecting = false;
    void (async () => {
      try {
        const connected = await TrellisClient.connect({
          trellisUrl: APP_CONFIG.authUrl,
          request: trellisApp.request,
          auth: consoleAuthOptions(),
          signal: abort.signal,
          onAuthRequired: ({ loginUrl }) => {
            redirecting = true;
            window.location.assign(loginUrl);
          },
        }).orThrow();
        await connected.connection.close();
        if (abort.signal.aborted) return;
        const target = sessionStorage.getItem(consoleReturnKey());
        sessionStorage.removeItem(consoleReturnKey());
        const destination = new URL(window.location.href);
        if (target) destination.searchParams.set("redirectTo", target);
        window.location.assign(getConsoleRedirectTarget(destination));
      } catch {
        if (!abort.signal.aborted && !redirecting) failed = true;
      }
    })();
    return () => abort.abort();
  });
</script>

<svelte:head><title>Sign in · Trellis</title></svelte:head>
<main class="portal-shell flex min-h-screen justify-center px-4 py-10 sm:px-6">
  <section class="w-full max-w-xl space-y-6">
    <PortalBrand subtitle="Console sign-in" />
    {#if failed}
      <p role="alert" class="portal-copy">
        Sign-in could not be completed. Start a new sign-in request.
      </p>
      <button
        class="btn btn-primary"
        onclick={() => window.location.assign(APP_CONFIG.callbackUri)}
        >Sign in again</button
      >
    {:else}
      <p role="status" class="portal-copy">Completing Console sign-in…</p>
      <span class="loading loading-spinner loading-sm" aria-hidden="true"
      ></span>
    {/if}
  </section>
</main>
