<script lang="ts">
  import type { Snippet } from "svelte";
  import {
    provideConnectedTrellisContext,
    type TrellisAppOwner,
    type TrellisContextClient,
  } from "../context.svelte.ts";

  type Props = {
    trellisApp: TrellisAppOwner;
    trellis: TrellisContextClient;
    children: Snippet;
  };

  const { trellisApp, trellis, children }: Props = $props();

  function installContext(): void {
    provideConnectedTrellisContext(trellisApp, trellis);
  }

  installContext();
</script>

{@render children()}
