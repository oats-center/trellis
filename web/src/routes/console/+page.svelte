<script lang="ts">
  import { onMount } from "svelte";
  import { browser } from "$app/environment";
  import { goto } from "$app/navigation";
  import { resolve } from "$app/paths";
  import LoadingScreen from "$lib/components/LoadingScreen.svelte";

  onMount(async () => {
    if (!browser) return;
    await goto(resolve("/console/profile"), { replaceState: true });
  });
</script>

<LoadingScreen />
