<script lang="ts">
  import { TrellisProvider } from "@oatscenter/trellis-svelte";
  import type { Snippet } from "svelte";
  import { onMount } from "svelte";
  import {
    setSelectedTrellisUrl,
    trellisApp,
  } from "$lib/trellis-context.svelte";
  import AuthenticatedApp from "$lib/components/AuthenticatedApp.svelte";
  import AdminAccountRedirect from "$lib/components/AdminAccountRedirect.svelte";
  import { buildConsoleLoginUrl, consoleReturnKey } from "$lib/auth";
  import {
    buildAdminAccountLoginUrl,
    consumeAdminAccountToken,
  } from "$lib/admin_account";
  import { APP_CONFIG, consoleAuthOptions } from "$lib/config";
  import { connectionProblem } from "$lib/connection_problem";

  type Props = {
    children: Snippet;
  };
  let { children }: Props = $props();
  let initialized = $state(false);
  let adminAccountToken = $state<string | null>(null);
  let showConnectingModal = $state(false);
  let connectingTimer: ReturnType<typeof setTimeout> | null = null;

  function currentPath(): string {
    return window.location.pathname + window.location.search;
  }

  onMount(() => {
    const currentUrl = new URL(window.location.href);
    adminAccountToken = consumeAdminAccountToken(currentUrl);
    if (adminAccountToken) {
      window.history.replaceState(window.history.state, "", currentUrl);
    }
    if (!APP_CONFIG.authUrl) {
      window.location.href = buildConsoleLoginUrl({
        redirectTo: currentPath(),
        location: window.location,
      });
      return;
    }

    setSelectedTrellisUrl(APP_CONFIG.authUrl);
    initialized = true;

    connectingTimer = setTimeout(() => {
      showConnectingModal = true;
    }, 4500);

    return () => {
      if (connectingTimer) clearTimeout(connectingTimer);
    };
  });

  function redirectToLogin(loginUrl: string): void {
    sessionStorage.setItem(consoleReturnKey(), currentPath());
    if (loginUrl) {
      if (adminAccountToken) {
        const setupUrl = buildAdminAccountLoginUrl(loginUrl, adminAccountToken);
        if (setupUrl) {
          adminAccountToken = null;
          window.location.href = setupUrl;
          return;
        }
      }
      window.location.href = loginUrl;
      return;
    }

    window.location.href = buildConsoleLoginUrl({
      redirectTo: currentPath(),
      location: window.location,
      authError: "Your session ended. Sign in again.",
    });
  }

  let recovering = $state(false);

  function recoverAuth(): void {
    if (recovering) return;
    recovering = true;
    window.location.href = buildConsoleLoginUrl({
      redirectTo: currentPath(),
      location: window.location,
    });
  }

  function goToConsole(): void {
    window.location.href = "/console";
  }

  function dismissConnecting(): void {
    if (connectingTimer) {
      clearTimeout(connectingTimer);
      connectingTimer = null;
    }
    showConnectingModal = false;
  }

  function connectingAction(): void {
    dismissConnecting();
  }
</script>

{#if initialized}
  <TrellisProvider
    {trellisApp}
    auth={consoleAuthOptions()}
    onAuthRequired={redirectToLogin}
    onRecoverableAuthError={recoverAuth}
  >
    {#snippet loading()}{/snippet}
    {#snippet recoveringAuth()}{/snippet}

    {#snippet error(connectError)}
      <div
        {@attach connectingAction}
        class="flex min-h-screen items-center justify-center bg-base-200 px-4 py-10"
      >
        <div class="flex flex-col items-center text-center">
          <svg
            class="mb-6 h-16 w-16 text-base-content/30"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M18 10h-4V6" />
            <path d="M14 10l7.03-7.03a2.12 2.12 0 0 1 3 3L17 13" />
            <path d="M6 14h4v4" />
            <path d="M10 14l-7.03 7.03a2.12 2.12 0 0 1-3-3L7 11" />
          </svg>
          <h1 class="text-xl font-semibold">Something went wrong</h1>
          <p class="mt-2 max-w-sm text-sm text-base-content/60">
            We couldn't load the console. This might be a temporary issue.
          </p>
          <p class="mt-1 max-w-sm text-sm text-base-content/60">
            {connectionProblem(connectError)}
          </p>
          <div class="mt-6 flex flex-col gap-3">
            <button class="btn btn-primary btn-sm" onclick={goToConsole}
              >Go to console</button
            >
          </div>
        </div>
      </div>
    {/snippet}

    {#if adminAccountToken}
      <div {@attach connectingAction}>
        <AdminAccountRedirect token={adminAccountToken} />
      </div>
    {:else}
      <AuthenticatedApp>
        <div {@attach connectingAction}>
          {@render children()}
        </div>
      </AuthenticatedApp>
    {/if}
  </TrellisProvider>

  {#if showConnectingModal}
    <dialog class="modal modal-open">
      <div class="modal-box">
        <div class="flex flex-col items-center gap-4 py-4">
          <span class="loading loading-spinner loading-lg"></span>
          <div class="text-center">
            <p class="text-sm font-medium">Connecting to Trellis</p>
            <p class="mt-1 text-xs text-base-content/60">
              This is taking longer than usual...
            </p>
          </div>
        </div>
      </div>
      <form method="dialog" class="modal-backdrop">
        <button>close</button>
      </form>
    </dialog>
  {/if}
{:else}
  <div
    class="flex min-h-screen flex-col items-center justify-center gap-3 bg-base-200 px-4 py-10 text-center"
  >
    <h1 class="text-lg font-semibold">Loading console</h1>
    <span class="loading loading-spinner loading-md"></span>
  </div>
{/if}
