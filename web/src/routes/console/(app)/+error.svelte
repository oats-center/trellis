<script lang="ts">
  type Props = {
    error: App.Error;
  };

  let { error }: Props = $props();

  function reload(): void {
    window.location.reload();
  }
</script>

<div class="flex min-h-screen items-center justify-center bg-base-200 px-4 py-10">
  <div class="w-full max-w-md text-center" data-testid="console-error-boundary">
    <h1 class="text-xl font-semibold">Something went wrong</h1>
    <p class="mt-2 text-sm text-base-content/60">
      The console hit an unexpected error while rendering this page.
    </p>
    <p class="mt-2 text-xs text-base-content/50">{error.message}</p>
    <div class="mt-6 flex justify-center gap-2">
      <button class="btn btn-outline btn-sm" type="button" onclick={reload}>Reload</button>
      <a class="btn btn-ghost btn-sm" href="/console/admin">Back to console</a>
    </div>
  </div>
</div>
