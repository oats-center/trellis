<script lang="ts">
  import PortalBrand from "$lib/components/PortalBrand.svelte";
</script>

<svelte:head><title>Authentication complete | Trellis</title></svelte:head>

<main class="portal-shell flex min-h-screen justify-center p-6" data-theme="portal">
  <div class="portal-page w-full max-w-lg">
    <header class="portal-header">
      <PortalBrand subtitle="Account" />
    </header>
    <div class="portal-body">
      <div>
        <p class="text-sm font-semibold uppercase tracking-wide text-success">Account ready</p>
        <h1 class="mt-2 text-3xl font-semibold text-base-content">Authentication complete</h1>
      </div>
      <p class="text-base-content/70">
        Your account change was saved. You can close this tab and return to the application that
        started the request.
      </p>
      <div>
        <a class="btn btn-primary" href="/console">Open Console</a>
      </div>
    </div>
  </div>
</main>
