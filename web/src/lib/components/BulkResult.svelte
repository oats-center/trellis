<script lang="ts">
  import Notice from "./Notice.svelte";

  type Props = {
    succeeded: number;
    failed: string[];
    pastTense: string;
    onRetry?: () => void;
    onDismiss?: () => void;
  };

  let { succeeded, failed, pastTense, onRetry, onDismiss }: Props = $props();
</script>

{#if failed.length > 0}
  <Notice variant="error">
    <div class="min-w-0">
      <p class="font-medium">
        {succeeded} {pastTense}, {failed.length} failed.
      </p>
      <ul class="trellis-identifier mt-1 max-h-40 space-y-0.5 overflow-y-auto text-xs">
        {#each failed as target (target)}
          <li>{target}</li>
        {/each}
      </ul>
      <div class="mt-2 flex gap-2">
        {#if onRetry && failed.length > 0}
          <button type="button" class="btn btn-outline btn-xs" onclick={onRetry}>Retry failed</button>
        {/if}
        {#if onDismiss}
          <button type="button" class="btn btn-ghost btn-xs" onclick={onDismiss}>Dismiss</button>
        {/if}
      </div>
    </div>
  </Notice>
{:else if succeeded > 0}
  <Notice variant="success">
    <p class="font-medium">{succeeded} {pastTense}.</p>
  </Notice>
{/if}
