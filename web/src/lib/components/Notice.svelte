<script lang="ts">
  import type { Snippet } from "svelte";
  import type { ClassValue } from "svelte/elements";

  type NoticeVariant = "error" | "warning" | "info" | "success";

  type Props = {
    children: Snippet;
    variant?: NoticeVariant;
    class?: ClassValue;
    role?: string;
  };

  let { children, variant = "info", class: className, role }: Props = $props();
</script>

<div class={[`alert alert-${variant}`, className]} {role}>
  <span>{@render children()}</span>
</div>
