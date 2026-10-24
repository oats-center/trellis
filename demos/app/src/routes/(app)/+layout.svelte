<script lang="ts">
  import type { Snippet } from "svelte";
  import { resolve } from "$app/paths";
  import { page } from "$app/state";
  import { TrellisProvider } from "@oatscenter/trellis-svelte";
  import { apps } from "../../../trellis/index.js";
  import AppShell from "$lib/components/AppShell.svelte";
  import { trellisApp } from "$lib/trellis";

  let { children }: { children: Snippet } = $props();
</script>

<TrellisProvider
  {trellisApp}
  auth={{
    clientId: apps.App.request.id,
    redirectTo: new URL(resolve("/inspection"), page.url.origin).href,
    loopbackDevelopment: true,
  }}
>
  {#snippet loading()}
    <section
      class="field-console mx-auto flex min-h-screen w-full max-w-6xl items-center justify-center px-4 py-10 sm:px-6 lg:px-8"
      aria-live="polite"
      aria-busy="true"
    >
      <div class="page-sheet w-full max-w-md rounded-box p-7">
        <div class="flex flex-col items-center gap-4 text-center" role="status">
          <p class="trellis-kicker">Field Inspection Desk</p>
          <span class="loading loading-spinner w-[3rem]"></span>
          <h1 class="text-lg font-bold tracking-tight">
            Connecting to Trellis
          </h1>
        </div>
      </div>
    </section>
  {/snippet}

  {#snippet error(cause)}
    <section
      class="field-console mx-auto flex min-h-screen w-full max-w-6xl items-center justify-center px-4 py-10 sm:px-6 lg:px-8"
    >
      <div class="page-sheet w-full max-w-xl rounded-box p-7">
        <div class="flex flex-col gap-5">
          <div class="space-y-2">
            <p class="trellis-kicker">Connection unavailable</p>
            <h1 class="text-lg font-black tracking-tight text-error">
              Trellis connection is unavailable
            </h1>
            <p class="text-sm leading-6 text-base-content/70">
              The Trellis server did not accept the demo connection. Confirm the
              demo server is running, then retry.
            </p>
          </div>

          <details
            class="border-y border-base-300/80 bg-base-200/55 px-1 py-3 text-xs"
          >
            <summary class="cursor-pointer font-semibold"
              >Show technical details</summary
            >
            <pre
              class="mt-3 overflow-x-auto whitespace-pre-wrap">{cause instanceof
              Error
                ? cause.message
                : "Connection failed"}</pre>
          </details>

          <a class="btn btn-outline btn-sm w-fit" href={resolve("/inspection")}
            >Retry</a
          >
        </div>
      </div>
    </section>
  {/snippet}

  <AppShell {children} />
</TrellisProvider>
