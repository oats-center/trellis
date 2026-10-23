<script lang="ts" module>
  /** The last view someone chose, shared by every data panel on the page. */
  const STORAGE_KEY = "trellis.console.dataView";
  let preferred = $state<"fields" | "json">(
    typeof localStorage !== "undefined" && localStorage.getItem(STORAGE_KEY) === "json" ? "json" : "fields",
  );
</script>

<script lang="ts">
  import DataView from "./DataView.svelte";
  import Icon from "./Icon.svelte";
  import { displayJson } from "$lib/console/display_value.ts";

  type Props = { value: unknown; label?: string };

  let { value, label = "Data" }: Props = $props();

  let copied = $state(false);
  const json = $derived(displayJson(value));

  type Token = { text: string; kind: "key" | "string" | "number" | "literal" | "plain" };

  // Splits formatted JSON into coloured tokens; anything unmatched stays plain.
  const tokens = $derived.by(() => {
    const out: Token[] = [];
    const pattern = /("(?:[^"\\]|\\.)*")(\s*:)?|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|\b(true|false|null)\b/g;
    let last = 0;
    for (const match of json.matchAll(pattern)) {
      const index = match.index ?? 0;
      if (index > last) out.push({ text: json.slice(last, index), kind: "plain" });
      if (match[1] !== undefined) {
        out.push({ text: match[1], kind: match[2] ? "key" : "string" });
        if (match[2]) out.push({ text: match[2], kind: "plain" });
      } else if (match[3] !== undefined) {
        out.push({ text: match[3], kind: "number" });
      } else {
        out.push({ text: match[0], kind: "literal" });
      }
      last = index + match[0].length;
    }
    if (last < json.length) out.push({ text: json.slice(last), kind: "plain" });
    return out;
  });

  function choose(view: "fields" | "json") {
    preferred = view;
    try {
      localStorage.setItem(STORAGE_KEY, view);
    } catch {
      // Private browsing may refuse storage; the choice still applies now.
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(json);
      copied = true;
      setTimeout(() => (copied = false), 1200);
    } catch {
      // Clipboard access can be refused; nothing else to do.
    }
  }
</script>

<div class="data-panel">
  <div class="data-panel-head">
    <h3 class="text-sm font-semibold">{label}</h3>
    <div class="flex items-center gap-2">
      {#if preferred === "json"}
        <button type="button" class="btn btn-ghost btn-xs" aria-label={`Copy ${label.toLowerCase()} as JSON`} onclick={() => void copy()}>
          <Icon name={copied ? "check" : "clipboard"} size={14} />
        </button>
      {/if}
      <div class="trellis-segment small" role="group" aria-label={`Show ${label.toLowerCase()} as`}>
        <button type="button" class:active={preferred === "fields"} aria-pressed={preferred === "fields"} onclick={() => choose("fields")}>Fields</button>
        <button type="button" class:active={preferred === "json"} aria-pressed={preferred === "json"} onclick={() => choose("json")}>JSON</button>
      </div>
    </div>
  </div>
  {#if preferred === "fields"}
    <DataView {value} />
  {:else}
    <pre class="json-code" aria-label={`${label} as JSON`}>{#each tokens as token, index (index)}<span class={`json-${token.kind}`}>{token.text}</span>{/each}</pre>
  {/if}
</div>

<style>
  .data-panel {
    display: grid;
    gap: 0.6rem;
    min-width: 0;
  }
  .data-panel-head {
    align-items: center;
    display: flex;
    gap: 0.75rem;
    justify-content: space-between;
  }
  .trellis-segment.small :global(button) {
    font-size: 0.75rem;
    padding: 0.2rem 0.6rem;
  }
  .json-code {
    background: var(--color-base-100);
    border: 1px solid var(--color-base-300);
    border-radius: var(--radius-field);
    font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace;
    font-size: 0.8125rem;
    line-height: 1.55;
    margin: 0;
    max-height: 28rem;
    overflow: auto;
    padding: 0.75rem 0.9rem;
    white-space: pre;
  }
  .json-key {
    color: var(--color-primary);
    font-weight: 600;
  }
  .json-string {
    color: var(--color-success);
  }
  .json-number {
    color: var(--color-info);
  }
  .json-literal {
    color: color-mix(in oklab, var(--color-secondary) 85%, var(--color-base-content));
    font-weight: 600;
  }
  .json-plain {
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
  }
</style>
