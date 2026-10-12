<script lang="ts">
  import type { TrellisConsoleClient } from "../trellis-context.svelte.ts";

  let { trellis }: { trellis: TrellisConsoleClient } = $props();
</script>

{#await trellis.usersResolve({}).orThrow() then account}
  <span>{account.user.displayName}</span>
  <span>{trellis.ownAuthority.current?.binding.kind}</span>
{/await}
