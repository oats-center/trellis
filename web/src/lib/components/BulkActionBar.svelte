<script lang="ts">
  import type { Snippet } from "svelte";

  type Props = {
    count: number;
    noun: string;
    actions: Snippet;
    onClear: () => void;
  };

  let { count, noun, actions, onClear }: Props = $props();
</script>

<div class="trellis-bulkbar" role="toolbar" aria-label="Bulk actions">
  <span class="text-sm font-medium">{count} {noun}{count === 1 ? "" : "s"} selected</span>
  <div class="flex items-center gap-2">
    {@render actions()}
    <button type="button" class="btn btn-ghost btn-sm" onclick={onClear}>Clear</button>
  </div>
</div>
