<script lang="ts">
  import { page } from "$app/state";
  import type { Snippet } from "svelte";
  import { onDestroy } from "svelte";
  import { buildConsoleLoginUrl } from "../auth";
  import {
    completeNavigation,
    currentNavigationToken,
  } from "../browser_telemetry";
  import { getVisibleNavSections, type NavSection } from "../control-panel.ts";
  import {
    ConsoleAuthority,
    provideConsoleAuthority,
  } from "../console/authority.svelte";
  import {
    NotificationsController,
    setNotifications,
  } from "../notifications.svelte";
  import { getConnection, getTrellis, type ConnectionStatus } from "../trellis";
  import { setChangeAccess } from "../console/change_access.ts";
  import { canPerform } from "../console/operations.ts";
  import AppShell, { type RuntimeVersion } from "./AppShell.svelte";
  import LoadingScreen from "./LoadingScreen.svelte";

  type Props = {
    children: Snippet;
  };

  let { children }: Props = $props();

  const connection = getConnection();
  const trellis = getTrellis();
  const notifications = setNotifications(new NotificationsController());

  // One authority object for the whole shell, published to context during
  // component initialization. It carries advisory identity/navigation metadata
  // and detects definitive session loss; it never decides whether a route
  // mounts or whether a request may be sent.
  const authority = new ConsoleAuthority(trellis);
  provideConsoleAuthority(authority);

  const connectionStatus = $derived<ConnectionStatus["phase"]>(
    connection.status.phase,
  );
  const snapshot = $derived(authority.snapshot);
  const navSections = $derived<NavSection[]>(
    getVisibleNavSections(snapshot.authority),
  );

  // A login redirect preserves the full current URL, including its query, so a
  // deep link such as a target action page survives sign-in.
  function redirectToLogin(): void {
    window.location.href = buildConsoleLoginUrl({
      redirectTo: `${page.url.pathname}${page.url.search}`,
      location: window.location,
    });
  }

  // Ending this app session sends the user back through consent, where their
  // earlier choices are preselected and can be changed.
  setChangeAccess(async () => {
    try {
      await trellis.logout();
    } catch (error) {
      notifications.error(
        error instanceof Error ? error.message : "Could not end this session.",
        "Change access failed",
      );
      return;
    }
    authority.clear();
    redirectToLogin();
  });

  // Signing out ends the Portal login too, so the next sign-in asks who is
  // signing in: the Console is often used on shared computers.
  async function signOut(): Promise<void> {
    try {
      await trellis.logout({
        endPortalLogin: true,
        returnTo: new URL(
          buildConsoleLoginUrl({ redirectTo: "/console/profile" }),
          window.location.href,
        ).href,
      });
    } catch (error) {
      notifications.error(
        error instanceof Error
          ? error.message
          : "Could not revoke this app session.",
        "Sign out failed",
      );
      return;
    }
    authority.clear();
  }

  // The server's version, and a newer release when an administrator asks. A
  // failed read only leaves the header without it.
  let runtimeVersion = $state<RuntimeVersion | null>(null);
  // The shell stays behind the loading screen until its first account check
  // settles, so it appears whole instead of filling in piece by piece.
  let revealed = $state(false);

  // The account check and the version read run together, so the header's
  // version comes with the shell. An administrator's read can wait on GitHub
  // for an hourly release check; the shell waits for it only briefly.
  function loadAuthority(): void {
    const token = currentNavigationToken();
    const version = canPerform(
      trellis.ownAuthority.current ?? null,
      "runtimeVersion",
    )
      ? trellis
          .runtimeVersion({})
          .orThrow()
          .then(
            (value) => {
              runtimeVersion = value;
            },
            () => undefined,
          )
      : undefined;
    void Promise.all([
      authority.reload(),
      version &&
        Promise.race([
          version,
          new Promise((resolve) => setTimeout(resolve, 750)),
        ]),
    ]).then(([result]) => {
      revealed = true;
      if (result.state === "auth-required") redirectToLogin();
      completeNavigation(token, result.state === "ready" ? "ok" : "error");
    });
  }

  // Exactly one initial Me request per usable connection, plus one after each
  // reconnection. Only a definitive expired/revoked/missing session redirects;
  // a recoverable failure stays a shell status with Retry.
  let wasConnected = false;

  $effect(() => {
    const connected = connectionStatus === "connected";
    if (!connected) {
      wasConnected = false;
      return;
    }
    if (wasConnected) return;
    wasConnected = true;
    loadAuthority();
  });

  onDestroy(() => {
    // Ends pending validations without dispatching more RPCs or redirects.
    authority.dispose();
    notifications.clear();
  });
</script>

<AppShell
  profile={snapshot.profile}
  authority={snapshot.authority}
  {navSections}
  {connectionStatus}
  {runtimeVersion}
  authFailure={snapshot.state === "error" &&
  snapshot.failure?.code !== "not_allowed"
    ? (snapshot.failure?.message ?? "Could not verify your session.")
    : null}
  accountNotAllowed={snapshot.state === "error" &&
    snapshot.failure?.code === "not_allowed"}
  onSignOut={signOut}
  onRetryAuthority={loadAuthority}
>
  <div data-testid="console-ready" class="contents">
    {@render children()}
  </div>
</AppShell>

{#if !revealed && connectionStatus === "connected"}
  <LoadingScreen />
{/if}
