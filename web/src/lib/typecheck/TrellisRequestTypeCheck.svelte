<script lang="ts">
  import type { TrellisConsoleClient } from "../trellis-context.svelte.ts";

  let { trellis }: { trellis: TrellisConsoleClient } = $props();

  function displayParticipantKind(
    kind: string,
  ): string {
    return kind;
  }

  function displayDeviceId(deviceId: string | undefined): string {
    return deviceId ?? "no-device";
  }
</script>

{#await trellis.sessionsMe({}).orThrow() then me}
  <span>{displayParticipantKind(me.connection.participantKind)}</span>
  <span>{displayDeviceId(me.connection.instanceId)}</span>
{/await}
