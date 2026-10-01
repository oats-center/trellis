<script lang="ts">
  import Icon from "./Icon.svelte";
  import { getNotifications } from "$lib/notifications.svelte";
  import { errorMessage } from "$lib/format";

  let { value, label = "Copy value" }: { value: string; label?: string } = $props();
  const notifications = getNotifications();
  let copied = $state<string | null>(null);
  let pending = $state(false);

  async function copy(): Promise<void> {
    pending = true;
    const text = value;
    try {
      if (!navigator.clipboard) throw new Error("Clipboard access is unavailable in this browser.");
      await navigator.clipboard.writeText(text);
      copied = text;
      notifications.success("Copied to clipboard.", "Copied");
    } catch (cause) { notifications.error(errorMessage(cause), "Copy failed"); }
    finally { pending = false; }
  }
</script>

<button type="button" class="btn btn-ghost btn-xs btn-square align-middle" aria-label={copied === value ? "Copied" : label} title={copied === value ? "Copied" : label} disabled={pending} onclick={() => void copy()}><Icon name={copied === value ? "check" : "clipboard"} size={14} /></button>
