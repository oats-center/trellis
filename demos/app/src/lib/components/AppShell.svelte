<script lang="ts">
  import { page } from "$app/state";
  import { goto } from "$app/navigation";
  import { resolve } from "$app/paths";
  import type { Snippet } from "svelte";
  import DemoClientLogo from "$lib/components/DemoClientLogo.svelte";
  import LiveEventStream from "$lib/components/LiveEventStream.svelte";
  import { getConnection } from "$lib/trellis";

  let { children }: { children: Snippet } = $props();

  const connection = getConnection();
  const currentPath = $derived.by((): string => page.url.pathname);
  const status = $derived(connection.status);
  const isConnected = $derived(status.phase === "connected");
  const connectionLabel = $derived.by((): string => {
    const phase = String(status.phase);
    if (isConnected) return "Connected";
    if (phase === "connecting" || phase === "loading") return "Connecting";
    if (phase === "failed" || phase === "error" || phase === "disconnected") return "Server unavailable";
    return "Authentication required";
  });
  const navItems = [
    { path: "/inspection", label: "Inspections", eyebrow: "Wizard workflow" },
    { path: "/reports", label: "Reports", eyebrow: "Completed closeouts" },
  ] as const;
  let loggingOut = $state(false);

  async function logout(): Promise<void> {
    loggingOut = true;
    try {
      await connection.close();
      await goto(resolve("/"));
    } finally {
      loggingOut = false;
    }
  }

</script>

<section class="app-shell field-console text-base-content">
  <a class="skip-workspace btn btn-accent btn-sm" href="#workspace-content">Skip to workspace</a>
  <aside class="app-sidebar flex min-w-0 flex-col px-4 py-5 lg:px-5">
    <div class="flex min-h-full flex-col gap-6">
      <a
        class="btn btn-ghost min-h-0 justify-start px-1 py-1 text-left hover:bg-base-200/70"
        href={resolve("/")}
        aria-label="Field Inspection Desk home"
      >
        <span class="sm:hidden"><DemoClientLogo variant="compact" /></span>
        <span class="hidden sm:inline-flex"><DemoClientLogo /></span>
      </a>

      <div class="section-rule pt-5">
        <p class="trellis-kicker">Inspection Desk</p>
        <h1 class="mt-1 text-lg font-black tracking-tight">Inspection command</h1>
        <span
          class={{
            "badge badge-outline mt-4 w-fit gap-1.5 px-2 py-2 text-[0.56rem] font-semibold uppercase tracking-[0.1em]": true,
            "badge-success": isConnected,
            "badge-warning": !isConnected,
          }}
        >
          <span class="size-1.5 rounded-full bg-current"></span>
          <span>{connectionLabel}</span>
        </span>
        {#if !isConnected}
          <a class="btn btn-accent btn-sm mt-3 w-full" href={resolve("/inspection")}>Sign in through demo flow</a>
        {/if}
      </div>

      <nav class="min-w-0" aria-label="Field Inspection Desk sections">
        <ul class="menu gap-1 p-0">
          {#each navItems as item (item.path)}
            <li>
              <a
                href={resolve(item.path)}
                class={[
                  "executive-nav-link",
                  currentPath === resolve(item.path) && "menu-active",
                ]}
                aria-current={currentPath === resolve(item.path) ? "page" : undefined}
              >
                <span class="flex min-w-0 flex-col gap-0.5">
                  <span class="break-words text-sm font-bold">{item.label}</span>
                  <span class="break-words text-[0.65rem] uppercase tracking-[0.18em] opacity-60">{item.eyebrow}</span>
                </span>
              </a>
            </li>
          {/each}
        </ul>
      </nav>

      <div class="mt-auto flex flex-col gap-3">
        {#if isConnected}
          <button
            class="btn btn-outline btn-sm w-full justify-start"
            disabled={loggingOut}
            onclick={() => void logout()}
          >
            {loggingOut ? "Signing out" : "Log out"}
          </button>
        {/if}
        <div class="capability-note">
          <strong>Trellis-backed:</strong>
          demo client
        </div>
      </div>
    </div>
  </aside>

  <main id="workspace-content" class="app-main px-4 py-5 sm:px-6 lg:px-8 lg:py-7" tabindex="-1">
    <div class="page-workspace mx-auto">
      {@render children()}
    </div>
  </main>

  <aside class="app-event-sidebar hidden min-w-0 xl:block">
    <LiveEventStream />
  </aside>
</section>
