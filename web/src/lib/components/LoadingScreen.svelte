<script lang="ts">
  import TrellisLogo from "./TrellisLogo.svelte";

  // The one loading presentation from the sign-in callback until the shell
  // shows: every step of the way draws the same screen, so nothing jumps.
</script>

<div
  class="fixed inset-0 z-[60] flex flex-col items-center justify-center gap-6 bg-base-200 px-4"
  role="status"
  aria-live="polite"
>
  <TrellisLogo />
  <span
    class="loading loading-spinner loading-md text-base-content/60"
    aria-hidden="true"
  ></span>
  <span class="sr-only">Loading Trellis</span>
</div>
