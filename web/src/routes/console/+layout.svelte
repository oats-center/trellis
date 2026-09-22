<script lang="ts">
  import { onMount } from "svelte";
  import { consoleTheme } from "$lib/theme.svelte";
  let { children } = $props();

  onMount(() => {
    consoleTheme.init();
  });
</script>

{@render children()}
