<script lang="ts">
  import { onMount } from "svelte";

  let { token }: { token: string } = $props();

  onMount(() => {
    const url = new URL("/login/admin/bootstrap", window.location.origin);
    url.searchParams.set("flowId", token);
    window.location.replace(url);
  });
</script>

<div class="flex min-h-screen items-center justify-center bg-base-200">
  <span class="loading loading-spinner loading-md"></span>
</div>
