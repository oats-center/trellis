<script lang="ts">
  import "../web.css";
  import GeneratedTlsBanner from "$lib/components/GeneratedTlsBanner.svelte";

  let { children } = $props();
</script>

<GeneratedTlsBanner />
{@render children()}
