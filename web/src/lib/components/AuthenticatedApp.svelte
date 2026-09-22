<script lang="ts">
  import { page } from "$app/state";
  import type { Snippet } from "svelte";
  import { onDestroy } from "svelte";
  import { buildConsoleLoginUrl } from "../auth";
  import {
    getVisibleNavSections,
    type NavSection,
  } from "../control-panel.ts";
  import {
    ConsoleAuthority,
    provideConsoleAuthority,
  } from "../console/authority.svelte";
  import { NotificationsController, setNotifications } from "../notifications.svelte";
  import { getConnection, getTrellis, type ConnectionStatus } from "../trellis";
  import AppShell from "./AppShell.svelte";

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

  const connectionStatus = $derived<ConnectionStatus["phase"]>(connection.status.phase);
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

  async function signOut(): Promise<void> {
    try {
      await trellis.logout();
    } finally {
      authority.clear();
      window.location.href = buildConsoleLoginUrl({ redirectTo: "/profile" });
    }
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
    void authority.reload().then((result) => {
      if (result.state === "auth-required") redirectToLogin();
    });
  });

  function retryAuthority(): void {
    void authority.reload().then((result) => {
      if (result.state === "auth-required") redirectToLogin();
    });
  }

  onDestroy(() => {
    // Ends pending validations without dispatching more RPCs or redirects.
    authority.dispose();
    notifications.clear();
  });
</script>

<AppShell
  profile={snapshot.profile}
  authority={snapshot.authority}
  profileLoaded={snapshot.state !== "checking"}
  {navSections}
  {connectionStatus}
  authFailure={snapshot.state === "error"
    ? snapshot.failure?.message ?? "Could not verify your session."
    : null}
  onSignOut={signOut}
  onRetryAuthority={retryAuthority}
>
  <div data-testid="console-ready" class="contents">
    {@render children()}
  </div>
</AppShell>
