<script lang="ts">
  import "../web.css";
  import GeneratedTlsBanner from "$lib/components/GeneratedTlsBanner.svelte";

  import { onMount } from "svelte";

  let { children } = $props();

  // The page's static loading screen covers the first frames; once the app
  // has drawn its own, it goes.
  onMount(() => document.getElementById("trellis-boot")?.remove());
</script>

<GeneratedTlsBanner />
{@render children()}
