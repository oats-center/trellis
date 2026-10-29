<script lang="ts">
  import DataView from "./DataView.svelte";
  import { sentenceName } from "$lib/console/plain_labels.ts";

  type Props = { value: unknown; depth?: number };

  let { value, depth = 0 }: Props = $props();

  // Data a service sent, shown as labelled fields rather than JSON.
  const MAX_DEPTH = 4;
  const MAX_ITEMS = 50;

  function isRecord(input: unknown): input is Record<string, unknown> {
    return input !== null && typeof input === "object" && !Array.isArray(input) && !(input instanceof Uint8Array);
  }

  function scalar(input: unknown): string {
    if (input === null || input === undefined) return "—";
    if (typeof input === "boolean") return input ? "Yes" : "No";
    if (input instanceof Uint8Array) return `${input.byteLength.toLocaleString()} bytes`;
    if (typeof input === "bigint" || typeof input === "number") return input.toLocaleString();
    return String(input);
  }

  function nested(input: unknown): boolean {
    return (isRecord(input) && Object.keys(input).length > 0) || (Array.isArray(input) && input.length > 0);
  }
</script>

{#if isRecord(value)}
  {@const entries = Object.entries(value)}
  {#if entries.length === 0}
    <p class="text-sm text-base-content/60">Empty</p>
  {:else}
    <dl class="data-view">
      {#each entries.slice(0, MAX_ITEMS) as [key, entry] (key)}
        <dt>{sentenceName(key)}</dt>
        <dd>
          {#if nested(entry) && depth < MAX_DEPTH}
            <DataView value={entry} depth={depth + 1} />
          {:else if nested(entry)}
            <span class="text-base-content/60">More detail</span>
          {:else}
            {scalar(entry)}
          {/if}
        </dd>
      {/each}
    </dl>
    {#if entries.length > MAX_ITEMS}<p class="text-xs text-base-content/60">
        And {entries.length - MAX_ITEMS} more fields
      </p>{/if}
  {/if}
{:else if Array.isArray(value)}
  {#if value.length === 0}
    <p class="text-sm text-base-content/60">None</p>
  {:else}
    <ol class="data-list">
      {#each value.slice(0, MAX_ITEMS) as entry, index (index)}
        <li>
          {#if nested(entry) && depth < MAX_DEPTH}
            <DataView value={entry} depth={depth + 1} />
          {:else}
            {scalar(entry)}
          {/if}
        </li>
      {/each}
    </ol>
    {#if value.length > MAX_ITEMS}<p class="text-xs text-base-content/60">
        And {value.length - MAX_ITEMS} more
      </p>{/if}
  {/if}
{:else}
  <p class="data-scalar">{scalar(value)}</p>
{/if}

<style>
  /* Nested values read as indented sections, not one run of fields. */
  .data-view :global(.data-view),
  .data-view :global(.data-list) {
    border-left: 2px solid var(--color-base-300);
    padding-left: 0.75rem;
  }
  .data-view > dt {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.8125rem;
  }
  .data-view {
    display: grid;
    gap: 0.35rem 1rem;
    grid-template-columns: minmax(7rem, max-content) minmax(0, 1fr);
    font-size: 0.875rem;
  }
  .data-view dt {
    color: color-mix(in oklab, var(--color-base-content) 62%, transparent);
  }
  .data-view dd {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .data-list {
    display: grid;
    gap: 0.4rem;
    font-size: 0.875rem;
    list-style: decimal;
    padding-left: 1.25rem;
  }
  .data-scalar {
    font-size: 0.875rem;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
</style>
