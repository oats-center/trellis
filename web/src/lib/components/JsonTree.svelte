<script lang="ts" module>
  type JsonValue =
    | string
    | number
    | boolean
    | null
    | undefined
    | JsonValue[]
    | { [key: string]: JsonValue };

  type Summary = {
    kind:
      | "object"
      | "array"
      | "string"
      | "number"
      | "bigint"
      | "bytes"
      | "boolean"
      | "null"
      | "empty";
    length?: number;
    preview?: string;
  };

  export function summarize(value: unknown): Summary {
    if (value === null) return { kind: "null" };
    if (value === undefined) return { kind: "empty" };
    if (value instanceof Uint8Array) {
      return { kind: "bytes", length: value.byteLength };
    }
    if (Array.isArray(value)) {
      if (value.length === 0) return { kind: "array", length: 0 };
      return { kind: "array", length: value.length };
    }
    const t = typeof value;
    if (t === "object") {
      const keys = Object.keys(value as Record<string, unknown>);
      if (keys.length === 0) return { kind: "object", length: 0 };
      return { kind: "object", length: keys.length };
    }
    if (t === "bigint") return { kind: "bigint" };
    if (t === "string") {
      const s = value as string;
      return {
        kind: "string",
        preview: s.length > 80 ? `${s.slice(0, 77)}…` : s,
      };
    }
    if (t === "number") return { kind: "number" };
    if (t === "boolean") return { kind: "boolean" };
    return { kind: "empty" };
  }
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";
  import JsonTree from "./JsonTree.svelte";
  import { displayJson } from "../console/display_value.ts";

  type Props = {
    value: unknown;
    label?: string;
    initiallyExpanded?: boolean;
    maxDepth?: number;
    visited?: Set<object>;
    fullscreenEnabled?: boolean;
    forceExpandStrings?: boolean;
  };

  let {
    value,
    label,
    initiallyExpanded = true,
    maxDepth = 4,
    visited,
    fullscreenEnabled = true,
    forceExpandStrings = false,
  }: Props = $props();

  const summary = $derived(summarize(value));
  const canExpand = $derived(
    summary.kind === "object" || summary.kind === "array",
  );
  const isCircular = $derived(
    canExpand && visited !== undefined && visited.has(value as object),
  );
  const isRoot = $derived(label === undefined);

  let expandedOverride = $state<boolean>();
  let stringExpandedOverride = $state<boolean>();
  let bytesExpanded = $state(false);
  let copyState = $state<"idle" | "copied" | "failed">("idle");
  const expanded = $derived(expandedOverride ?? initiallyExpanded);
  const stringExpanded = $derived(stringExpandedOverride ?? forceExpandStrings);
  let fullscreenOpen = $state(false);
  let dialog: HTMLDialogElement | undefined = $state();

  $effect(() => {
    initiallyExpanded;
    expandedOverride = undefined;
  });

  $effect(() => {
    forceExpandStrings;
    stringExpandedOverride = undefined;
  });

  $effect(() => {
    if (fullscreenOpen && dialog) {
      if (!dialog.open) dialog.showModal();
    } else if (!fullscreenOpen && dialog) {
      if (dialog.open) dialog.close();
    }
  });

  function toggle() {
    if (!canExpand) return;
    expandedOverride = !expanded;
  }

  function toggleString() {
    if (summary.kind !== "string") return;
    stringExpandedOverride = !stringExpanded;
  }

  function openFullscreen() {
    fullscreenOpen = true;
  }

  function closeFullscreen() {
    fullscreenOpen = false;
  }

  async function copyChild() {
    const text = displayJson(value);
    if (typeof navigator === "undefined" || !navigator.clipboard) {
      copyState = "failed";
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      copyState = "copied";
    } catch {
      copyState = "failed";
    }
    setTimeout(() => {
      copyState = "idle";
    }, 1_500);
  }

  function bytesPreview(bytes: Uint8Array): string {
    const shown = bytes.slice(0, 24);
    return [...shown].map((byte) => byte.toString(16).padStart(2, "0")).join(" ") +
      (bytes.length > shown.length ? " …" : "");
  }

  function asArray(value: unknown): unknown[] {
    if (!Array.isArray(value)) return [];
    return value;
  }

  function asObject(value: unknown): Array<[string, unknown]> {
    if (value === null || typeof value !== "object" || Array.isArray(value)) return [];
    return Object.entries(value as Record<string, unknown>);
  }

  function addToVisited(set: Set<object> | undefined, value: unknown): Set<object> {
    const next = new Set<object>(set);
    if (value !== null && typeof value === "object") {
      next.add(value);
    }
    return next;
  }
</script>

<div class="json-tree" data-depth={0}>
  <div class="json-tree-row">
    {#if canExpand}
      <button
        type="button"
        class="json-tree-toggle"
        aria-expanded={expanded}
        aria-label={expanded ? "Collapse" : "Expand"}
        onclick={toggle}
      >
        <span class="json-tree-caret" class:open={expanded}>▸</span>
      </button>
    {:else}
      <span class="json-tree-toggle json-tree-toggle-empty" aria-hidden="true"></span>
    {/if}
    {#if label !== undefined}
      <span class="json-tree-key">{label}</span>
      <span class="json-tree-punct">:</span>
    {/if}
    <span class={["json-tree-pill", `json-tree-pill-${summary.kind}`]}>
      {#if isCircular}
        [Circular reference]
      {:else if summary.kind === "object"}
        {`{ ${summary.length ?? 0} ${summary.length === 1 ? "key" : "keys"} }`}
      {:else if summary.kind === "array"}
        {`[ ${summary.length ?? 0} ${summary.length === 1 ? "item" : "items"} ]`}
      {:else if summary.kind === "string"}
        "{stringExpanded && typeof value === "string" ? value : (summary.preview ?? "")}"
      {:else if summary.kind === "bigint"}
        {String(value)}
      {:else if summary.kind === "bytes"}
        {`${summary.length ?? 0} bytes`}
      {:else if summary.kind === "number"}
        {String(value)}
      {:else if summary.kind === "boolean"}
        {String(value)}
      {:else if summary.kind === "null"}
        null
      {/if}
    </span>
    {#if summary.kind === "bytes" && value instanceof Uint8Array}
      <button
        type="button"
        class="json-tree-expand"
        aria-label={bytesExpanded ? "Hide bytes" : "Show bytes"}
        onclick={() => { bytesExpanded = !bytesExpanded; }}
      >
        {bytesExpanded ? "hide" : "show"}
      </button>
      {#if bytesExpanded}
        <span class="json-tree-bytes">{bytesPreview(value)}</span>
      {/if}
    {/if}
    {#if summary.kind === "string" && typeof value === "string" && value.length > 80}
      <button
        type="button"
        class="json-tree-expand"
        aria-label={stringExpanded ? "Collapse string" : "Expand string"}
        onclick={toggleString}
      >
        {stringExpanded ? "less" : "more"}
      </button>
    {/if}
    {#if canExpand && initiallyExpanded}
      {#if fullscreenEnabled && isRoot}
        <button
          type="button"
          class="json-tree-fullscreen"
          aria-label="View at full width"
          onclick={openFullscreen}
        >
          <Icon name="expand" size={12} />
          <span>Full width</span>
        </button>
      {/if}
      <button
        type="button"
        class="json-tree-copy"
        aria-label="Copy this value as JSON"
        onclick={copyChild}
      >
        <Icon name="clipboard" size={10} />
        {#if copyState === "copied"}<span class="json-tree-copy-state">Copied</span>{:else if copyState === "failed"}<span class="json-tree-copy-state">Copy unavailable</span>{/if}
      </button>
    {/if}
  </div>
  {#if canExpand && expanded && !isCircular}
    {@const childVisited = addToVisited(visited, value)}
    <div class="json-tree-children">
      {#if summary.kind === "array"}
        {#each asArray(value) as item, index (index)}
          <JsonTree
            value={item}
            label={`${index}`}
            initiallyExpanded={initiallyExpanded && maxDepth > 1}
            maxDepth={maxDepth - 1}
            visited={childVisited}
            {fullscreenEnabled}
            {forceExpandStrings}
          />
        {/each}
      {:else}
        {#each asObject(value) as [key, child] (key)}
          <JsonTree
            value={child}
            label={key}
            initiallyExpanded={initiallyExpanded && maxDepth > 1}
            maxDepth={maxDepth - 1}
            visited={childVisited}
            {fullscreenEnabled}
            {forceExpandStrings}
          />
        {/each}
      {/if}
    </div>
  {/if}
</div>

{#if fullscreenEnabled && fullscreenOpen}
  <dialog bind:this={dialog} class="modal" onclose={closeFullscreen} aria-labelledby="json-fullscreen-title">
    <div class="modal-box json-fullscreen-box border border-base-300 bg-base-100 max-w-[min(96vw,120rem)]">
      <div class="json-fullscreen-header">
        <h3 id="json-fullscreen-title" class="json-fullscreen-title">
          {label !== undefined ? `JSON: ${label}` : "JSON"}
        </h3>
        <button
          type="button"
          class="json-fullscreen-close"
          aria-label="Close"
          onclick={closeFullscreen}
        >
          <Icon name="close" size={12} />
        </button>
      </div>
      <div class="json-fullscreen-content">
        <JsonTree
          {value}
          initiallyExpanded={true}
          maxDepth={20}
          fullscreenEnabled={false}
          forceExpandStrings={true}
        />
      </div>
    </div>
    <form method="dialog" class="modal-backdrop">
      <button type="submit" aria-label="Close">close</button>
    </form>
  </dialog>
{/if}

<style>
  .json-tree {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    font-size: 0.78rem;
    line-height: 1.5;
  }

  .json-tree-row {
    align-items: center;
    display: flex;
    gap: 0.3rem;
    min-width: 0;
    padding: 0.1rem 0;
  }

  .json-tree-bytes {
    color: color-mix(in oklab, var(--color-base-content) 55%, transparent);
    font-size: 0.7rem;
    overflow-wrap: anywhere;
  }

  .json-tree-copy-state {
    font-size: 0.6rem;
    margin-left: 0.2rem;
  }

  .json-tree-toggle {
    align-items: center;
    background: transparent;
    border: none;
    color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
    cursor: pointer;
    display: inline-flex;
    justify-content: center;
    padding: 0;
    width: 1rem;
  }

  .json-tree-toggle-empty {
    cursor: default;
  }

  .json-tree-caret {
    color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
    display: inline-block;
    font-size: 0.65rem;
    transition: transform 150ms ease-out;
  }

  .json-tree-caret.open {
    transform: rotate(90deg);
  }

  .json-tree-key {
    color: var(--color-base-content);
    overflow-wrap: break-word;
  }

  .json-tree-punct {
    color: color-mix(in oklab, var(--color-base-content) 50%, transparent);
  }

  .json-tree-pill {
    align-items: baseline;
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
    display: block;
    font-size: 0.72rem;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .json-tree-pill-string {
    color: oklch(0.6 0.16 145);
  }

  .json-tree-pill-number,
  .json-tree-pill-boolean {
    color: oklch(0.62 0.16 245);
  }

  .json-tree-pill-null,
  .json-tree-pill-empty {
    color: oklch(0.6 0.18 25);
  }

  .json-tree-copy {
    background: transparent;
    border: none;
    color: color-mix(in oklab, var(--color-base-content) 40%, transparent);
    cursor: pointer;
    display: inline-flex;
    margin-left: 0.2rem;
    opacity: 0;
    padding: 0;
    transition: opacity 150ms ease-out, color 150ms ease-out;
  }

  .json-tree-row:hover .json-tree-copy,
  .json-tree-copy:focus-visible {
    opacity: 1;
  }

  .json-tree-copy:hover {
    color: var(--color-base-content);
  }

  .json-tree-copy:focus-visible {
    outline: 2px solid var(--color-accent);
    outline-offset: 1px;
  }

  .json-tree-fullscreen {
    align-items: center;
    background: transparent;
    border: none;
    color: color-mix(in oklab, var(--color-base-content) 45%, transparent);
    cursor: pointer;
    display: inline-flex;
    font-family: inherit;
    font-size: 0.65rem;
    gap: 0.25rem;
    margin-left: 0.3rem;
    padding: 0.1rem 0.35rem;
    border-radius: 0.25rem;
    transition: background 150ms ease-out, color 150ms ease-out;
  }

  .json-tree-fullscreen:hover {
    background: color-mix(in oklab, var(--color-base-content) 8%, transparent);
    color: var(--color-base-content);
  }

  .json-tree-fullscreen:focus-visible {
    outline: 2px solid var(--color-accent);
    outline-offset: 1px;
  }

  .json-fullscreen-box {
    max-height: 85vh;
    overflow: auto;
    padding: 0;
  }

  .json-fullscreen-header {
    align-items: center;
    border-bottom: 1px solid var(--color-base-300);
    display: flex;
    justify-content: space-between;
    padding: 0.75rem 1rem;
    position: sticky;
    top: 0;
    background: var(--color-base-100);
    z-index: 1;
  }

  .json-fullscreen-title {
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 0.85rem;
    font-weight: 600;
    margin: 0;
  }

  .json-fullscreen-close {
    background: transparent;
    border: none;
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    cursor: pointer;
    display: inline-flex;
    padding: 0.25rem;
  }

  .json-fullscreen-close:hover {
    color: var(--color-base-content);
  }

  .json-fullscreen-content {
    padding: 1rem;
  }

  .json-tree-expand {
    background: transparent;
    border: none;
    color: color-mix(in oklab, var(--color-base-content) 40%, transparent);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.65rem;
    margin-left: 0.2rem;
    opacity: 0;
    padding: 0;
    transition: opacity 150ms ease-out, color 150ms ease-out;
  }

  .json-tree-row:hover .json-tree-expand,
  .json-tree-expand:focus-visible {
    opacity: 1;
  }

  .json-tree-expand:hover {
    color: var(--color-base-content);
  }

  .json-tree-expand:focus-visible {
    outline: 2px solid var(--color-accent);
    outline-offset: 1px;
  }

  .json-tree-children {
    border-left: 1px solid color-mix(in oklab, var(--color-base-300) 50%, transparent);
    margin-left: 0.5rem;
    padding-left: 0.65rem;
  }
</style>
