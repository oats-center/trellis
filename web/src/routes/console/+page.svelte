<script lang="ts">
  import { onMount } from "svelte";
  import { browser } from "$app/environment";
  import { goto } from "$app/navigation";
  import { resolve } from "$app/paths";

  onMount(async () => {
    if (!browser) return;
    await goto(resolve("/console/profile"));
  });
</script>

<div
  class="flex min-h-screen flex-col items-center justify-center gap-3 bg-base-200 px-4 text-center"
>
  <h1 class="text-lg font-semibold">Loading console</h1>
  <span class="loading loading-spinner loading-md"></span>
</div>
