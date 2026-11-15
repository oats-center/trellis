<script lang="ts">
  import Icon from "./Icon.svelte";
  import {
    asSchema,
    definitionsOf,
    descriptionOf,
    fieldsOf,
    typeLabel,
    type Schema,
  } from "$lib/console/schema.ts";

  // With a title, the schema is its own card: the title and root type in a
  // header bar, fields below. Without one it renders inline, for nesting.
  let { schema, title }: { schema: unknown; title?: string } = $props();
  const root = $derived(asSchema(schema));
  const defs = $derived(definitionsOf(schema));
  let open = $state<string[]>([]);

  function toggle(path: string): void {
    open = open.includes(path)
      ? open.filter((value) => value !== path)
      : [...open, path];
  }

  // A recursive model stops opening at the type it is already inside.
  function refOf(node: Schema): string | null {
    return typeof node.$ref === "string" ? node.$ref : null;
  }
</script>

{#snippet fields(node: Schema, path: string, inside: string[], depth: number)}
  {@const items = fieldsOf(node, defs) ?? []}
  <ul class="schema-fields" class:nested={depth > 0}>
    {#each items as field (field.name)}
      {@const key = `${path}.${field.name}`}
      {@const nested = fieldsOf(field.schema, defs)}
      {@const ref = refOf(field.schema)}
      {@const expandable =
        !!nested?.length && !(ref !== null && inside.includes(ref))}
      {@const expanded = open.includes(key)}
      {@const description =
        field.description ?? descriptionOf(field.schema, defs)}
      <li>
        <div class="schema-row">
          <span class="schema-name">
            {#if expandable}
              <button
                type="button"
                class="schema-toggle"
                aria-expanded={expanded}
                aria-label={`${expanded ? "Hide" : "Show"} ${field.name} fields`}
                onclick={() => toggle(key)}
                ><Icon
                  name="chevronDown"
                  size={14}
                  class={expanded ? "" : "-rotate-90"}
                /></button
              >
            {:else}<span class="schema-toggle" aria-hidden="true"></span>{/if}
            <code>{field.name}</code>
            {#if !field.required}<span class="schema-optional">optional</span
              >{/if}
          </span>
          <span class="schema-type">{typeLabel(field.schema, defs)}</span>
          {#if description}<p class="schema-description">{description}</p>{/if}
        </div>
        {#if expandable && expanded}
          {@render fields(
            field.schema,
            key,
            ref === null ? inside : [...inside, ref],
            depth + 1,
          )}
        {/if}
      </li>
    {/each}
  </ul>
{/snippet}

{#snippet body(node: Schema)}
  {#if fieldsOf(node, defs)?.length}
    {@render fields(node, "", refOf(node) ? [refOf(node) ?? ""] : [], 0)}
  {:else if fieldsOf(node, defs)}
    <p class="schema-empty">No fields.</p>
  {/if}
{/snippet}

{#if root}
  {@const description = descriptionOf(root, defs)}
  {#if title}
    <section class="schema-card">
      <header class="schema-header">
        <h3>{title}</h3>
        <span class="schema-root">{typeLabel(root, defs)}</span>
      </header>
      {#if description}<p class="schema-summary">{description}</p>{/if}
      {@render body(root)}
    </section>
  {:else}
    <div class="schema-inline">
      <span class="schema-root">{typeLabel(root, defs)}</span>
      {#if description}<span class="schema-summary-inline">
          · {description}</span
        >{/if}
      {@render body(root)}
    </div>
  {/if}
{/if}

<style>
  .schema-card {
    background: var(--color-base-100);
    border: 1px solid var(--color-base-300);
    border-radius: 0.75rem;
    font-size: 0.875rem;
    overflow: hidden;
  }
  .schema-header {
    align-items: baseline;
    background: color-mix(in oklab, var(--color-base-200) 70%, transparent);
    border-bottom: 1px solid var(--color-base-300);
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
    justify-content: space-between;
    padding: 0.55rem 0.9rem;
  }
  .schema-header h3 {
    font-size: 0.8125rem;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: uppercase;
  }
  .schema-root {
    color: color-mix(in oklab, var(--color-base-content) 65%, transparent);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
  }
  .schema-summary {
    border-bottom: 1px solid var(--color-base-300);
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
    padding: 0.5rem 0.9rem;
  }
  .schema-inline {
    font-size: 0.875rem;
  }
  .schema-summary-inline {
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
  }
  .schema-empty {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    padding: 0.55rem 0.9rem;
  }
  .schema-fields > li + li {
    border-top: 1px solid var(--color-base-300);
  }
  .schema-fields.nested {
    background: color-mix(in oklab, var(--color-base-200) 45%, transparent);
    border-top: 1px solid var(--color-base-300);
    padding-left: 1.4rem;
  }
  .schema-inline > .schema-fields {
    border: 1px solid var(--color-base-300);
    border-radius: 0.5rem;
    margin-top: 0.4rem;
  }
  .schema-row {
    align-items: baseline;
    column-gap: 1rem;
    display: grid;
    grid-template-columns: minmax(10rem, 2fr) minmax(0, 3fr);
    padding: 0.45rem 0.9rem;
  }
  .schema-name {
    align-items: center;
    display: inline-flex;
    gap: 0.35rem;
    min-width: 0;
  }
  .schema-name code {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .schema-type {
    color: color-mix(in oklab, var(--color-base-content) 62%, transparent);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
  }
  .schema-optional {
    border: 1px solid var(--color-base-300);
    border-radius: 0.375rem;
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.6875rem;
    line-height: 1.3;
    padding: 0 0.3rem;
  }
  .schema-description {
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
    grid-column: 1 / -1;
    margin: 0.15rem 0 0 1.45rem;
    white-space: pre-line;
  }
  .schema-toggle {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 0.25rem;
    cursor: pointer;
    display: inline-flex;
    flex: none;
    height: 1.1rem;
    justify-content: center;
    width: 1.1rem;
  }
  span.schema-toggle {
    cursor: default;
  }
  button.schema-toggle:hover {
    background: var(--color-base-200);
  }
  @media (max-width: 40rem) {
    .schema-row {
      grid-template-columns: minmax(0, 1fr);
    }
    .schema-type {
      margin-left: 1.45rem;
    }
  }
</style>
