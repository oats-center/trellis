<script lang="ts">
  import { onMount } from "svelte";
  import { goto, preloadCode } from "$app/navigation";
  import { isErr } from "@oatscenter/result";
  import { TrellisClient } from "@oatscenter/trellis";
  import { APP_CONFIG, consoleAuthOptions } from "$lib/config";
  import { consoleReturnKey, getConsoleRedirectTarget } from "$lib/auth";
  import { base } from "$lib/console_paths";
  import { signInHandoff, trellisApp } from "$lib/trellis-context.svelte";
  import LoadingScreen from "$lib/components/LoadingScreen.svelte";
  import TrellisLogo from "$lib/components/TrellisLogo.svelte";
  import { connectionProblem } from "$lib/connection_problem";

  let failed = $state(false);
  let problem = $state("");
  let restarting = $state(false);

  // Retrying with the same app session and Portal login would fail the same
  // way, so starting over ends both and asks who is signing in.
  async function restart(): Promise<void> {
    restarting = true;
    const ended = (
      await TrellisClient.signOut({
        trellisUrl: APP_CONFIG.authUrl,
        auth: consoleAuthOptions(),
        returnTo: APP_CONFIG.callbackUri,
      })
    ).take();
    if (isErr(ended)) {
      problem = connectionProblem(ended.error);
      restarting = false;
    }
  }

  onMount(() => {
    // A requested destination must survive the round trip to the
    // authorization server, which returns to this page without it. Only a
    // same-origin Console target is kept, and it stays until sign-in succeeds
    // so a retry still returns there.
    const requested = new URL(window.location.href);
    if (requested.searchParams.has("redirectTo"))
      sessionStorage.setItem(
        consoleReturnKey(),
        getConsoleRedirectTarget(requested),
      );
    const target = sessionStorage.getItem(consoleReturnKey());
    const destination = new URL(window.location.href);
    if (target) destination.searchParams.set("redirectTo", target);
    const next = getConsoleRedirectTarget(destination);
    const path = new URL(next, window.location.href).pathname;
    // A Console page opens in this document with the session connected here:
    // no second load, key exchange, or connection. Its code loads meanwhile.
    const inConsole =
      path.startsWith(`${base}/`) && !path.startsWith(`${base}/login`);
    if (inConsole && requested.searchParams.has("code"))
      preloadCode(path).catch(() => undefined);
    const abort = new AbortController();
    let redirecting = false;
    let handedOff = false;
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
        if (abort.signal.aborted) {
          await connected.connection.close();
          return;
        }
        sessionStorage.removeItem(consoleReturnKey());
        // Replacing the callback keeps Back from returning to its spent code.
        if (inConsole) {
          handedOff = true;
          signInHandoff.client = connected;
          await goto(next, { replaceState: true });
          return;
        }
        await connected.connection.close();
        window.location.replace(next);
      } catch (error) {
        if (abort.signal.aborted || redirecting) return;
        problem = connectionProblem(error);
        failed = true;
      }
    })();
    // The connection belongs to the shell once handed off.
    return () => {
      if (!handedOff) abort.abort();
    };
  });
</script>

<svelte:head><title>Sign in · Trellis</title></svelte:head>
{#if failed}
  <main
    class="portal-shell flex min-h-screen justify-center px-4 py-10 sm:px-6"
  >
    <section class="w-full max-w-xl space-y-6">
      <TrellisLogo />
      <p role="alert" class="portal-copy">
        Sign-in could not be completed. Start a new sign-in request.
      </p>
      {#if problem}<p class="portal-copy text-sm">{problem}</p>{/if}
      <button class="btn btn-primary" disabled={restarting} onclick={restart}
        >Sign in again</button
      >
    </section>
  </main>
{:else}
  <LoadingScreen />
{/if}
