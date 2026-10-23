<script lang="ts">
  import Icon from "./Icon.svelte";

  type Props = {
    /** What is shown, e.g. "Showing 12 services". */
    summary: string;
    canPrevious: boolean;
    canNext: boolean;
    busy?: boolean;
    onPrevious: () => void;
    onNext: () => void;
  };

  let {
    summary,
    canPrevious,
    canNext,
    busy = false,
    onPrevious,
    onNext,
  }: Props = $props();
</script>

<div
  class="flex items-center justify-between gap-2 border-t border-base-300 px-4 py-3"
>
  <span class="text-sm text-base-content/60">{summary}</span>
  <div class="flex gap-2">
    <button
      class="btn btn-sm btn-square btn-outline"
      aria-label="Previous page"
      onclick={onPrevious}
      disabled={!canPrevious || busy}
      ><Icon name="chevronDown" size={16} class="rotate-90" /></button
    >
    <button
      class="btn btn-sm btn-square btn-outline"
      aria-label="Next page"
      onclick={onNext}
      disabled={!canNext || busy}
      ><Icon name="chevronDown" size={16} class="-rotate-90" /></button
    >
  </div>
</div>
