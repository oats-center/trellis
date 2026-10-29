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

  let { schema }: { schema: unknown } = $props();
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

{#snippet fields(node: Schema, path: string, inside: string[])}
  {@const items = fieldsOf(node, defs) ?? []}
  <ul class="schema-fields">
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
          <code class="font-medium">{field.name}</code>
          <span class="text-base-content/65"
            >{typeLabel(field.schema, defs)}</span
          >
          {#if !field.required}<span class="badge badge-ghost badge-xs"
              >optional</span
            >{/if}
        </div>
        {#if description}<p class="schema-description">{description}</p>{/if}
        {#if expandable && expanded}
          {@render fields(
            field.schema,
            key,
            ref === null ? inside : [...inside, ref],
          )}
        {/if}
      </li>
    {/each}
  </ul>
{/snippet}

{#if root}
  {@const description = descriptionOf(root, defs)}
  <div class="text-sm">
    <p>
      <code class="font-medium">{typeLabel(root, defs)}</code>
      {#if description}<span class="text-base-content/70">
          · {description}</span
        >{/if}
    </p>
    {#if fieldsOf(root, defs)?.length}
      {@render fields(root, "", refOf(root) ? [refOf(root) ?? ""] : [])}
    {:else if fieldsOf(root, defs)}
      <p class="mt-1 text-base-content/60">No fields.</p>
    {/if}
  </div>
{/if}

<style>
  .schema-fields {
    border-left: 1px solid var(--color-base-300);
    margin: 0.4rem 0 0 0.45rem;
    padding-left: 0.6rem;
  }
  .schema-fields > li {
    padding: 0.2rem 0;
  }
  .schema-row {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.5rem;
  }
  .schema-toggle {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 0.25rem;
    cursor: pointer;
    display: inline-flex;
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
  .schema-description {
    color: color-mix(in oklab, var(--color-base-content) 70%, transparent);
    margin-left: 1.6rem;
    white-space: pre-line;
  }
</style>
