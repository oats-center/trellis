<script lang="ts">
  import Icon from "./Icon.svelte";
  import SettingValue from "./SettingValue.svelte";
  import type { Schema } from "$lib/console/schema.ts";
  import {
    JsonText,
    emptyValue,
    fieldLabel,
    fromLocalInput,
    problemOf,
    shapeOf,
    toLocalInput,
    type Draft,
    type Shape,
  } from "$lib/console/settings.ts";

  // One value edited in the input its type calls for: a switch, a number
  // within its bounds, a list of typed rows, a model's own fields, and JSON
  // only for a type with no fixed shape. Lists, maps, and models hold one of
  // these per item, entry, or field.
  type Props = {
    schema: Schema;
    defs: Record<string, Schema>;
    value: Draft;
    onchange: (value: Draft) => void;
    id: string;
    /** The accessible name of the input, or of the group for a list or model. */
    label: string;
    disabled?: boolean;
    /** Show what is wrong with the value; off until it is edited. */
    checked?: boolean;
    /** Offer a control that stores null where the type allows it. A setting
     * turns it off: its own Clear stores null. */
    clearable?: boolean;
    /** Shown in an empty text or number input, such as where a value comes from. */
    placeholder?: string;
    /** Show the type's bounds beside the input; a list says them once. */
    hinted?: boolean;
    depth?: number;
  };
  let {
    schema,
    defs,
    value,
    onchange,
    id,
    label,
    disabled = false,
    checked = false,
    clearable = true,
    placeholder = "",
    hinted = true,
    depth = 0,
  }: Props = $props();

  const OTHER = "\u0000other";
  const shape = $derived(shapeOf(schema, defs, depth));
  // A nullable value edits as its non-null type; text shows null as empty.
  const inner = $derived(
    shape.kind === "nullable" ? shapeOf(shape.inner, defs, depth + 1) : shape,
  );
  const problem = $derived(
    checked ? problemOf(schema, defs, value, depth) : null,
  );
  const problemId = $derived(`${id}-problem`);
  const size = $derived(depth === 0 ? "" : "input-sm");
  const items = $derived(Array.isArray(value) ? value : []);
  const isRecord = (current: Draft): current is { [key: string]: Draft } =>
    typeof current === "object" &&
    current !== null &&
    !Array.isArray(current) &&
    !(current instanceof JsonText);
  const record = $derived(isRecord(value) ? value : {});
  // A model not entered yet starts with its required lists, models, and
  // switches in place, so they can be filled in.
  const fields = $derived.by(() => {
    if (isRecord(value)) return value;
    const start = value === undefined ? emptyValue(schema, defs, depth) : {};
    return isRecord(start) ? start : {};
  });

  function bounds(kind: Shape): string {
    if (kind.kind === "integer" || kind.kind === "bigint")
      return kind.narrowed ? `From ${kind.min} to ${kind.max}` : "";
    if (kind.kind === "number") {
      if (kind.min !== undefined && kind.max !== undefined)
        return `From ${kind.min} to ${kind.max}`;
      if (kind.min !== undefined) return `At least ${kind.min}`;
      if (kind.max !== undefined) return `At most ${kind.max}`;
    }
    if (kind.kind === "text") {
      if (kind.format === "bytes") return "Base64";
      if (kind.maxLength !== undefined)
        return `Up to ${kind.maxLength} characters`;
    }
    return "";
  }

  function jsonText(current: Draft): string {
    if (current instanceof JsonText) return current.text;
    return current === undefined ? "" : JSON.stringify(current, null, 2);
  }

  function parseJson(text: string): Draft {
    if (text.trim() === "") return undefined;
    try {
      return JSON.parse(text) as Draft;
    } catch {
      return new JsonText(text);
    }
  }

  function moved(index: number, by: number): Draft[] {
    const next = [...items];
    const [item] = next.splice(index, 1);
    next.splice(index + by, 0, item);
    return next;
  }

  // Renaming an entry keeps its place; a name already in use is refused.
  let clash = $state<Record<number, boolean>>({});
  function renamed(index: number, name: string): void {
    const entries = Object.entries(record);
    if (entries.some(([key], other) => key === name && other !== index)) {
      clash = { ...clash, [index]: true };
      return;
    }
    clash = { ...clash, [index]: false };
    onchange(
      Object.fromEntries(
        entries.map(([key, item], other) =>
          other === index ? [name, item] : [key, item],
        ),
      ),
    );
  }
</script>

{#snippet hint(text: string)}
  {#if text && hinted}<span
      class="text-xs whitespace-nowrap text-base-content/55">{text}</span
    >{/if}
{/snippet}

{#snippet leaf(kind: Shape)}
  {#if kind.kind === "boolean"}
    <label class="flex w-fit cursor-pointer items-center gap-2 py-1.5">
      <input
        {id}
        type="checkbox"
        class="toggle toggle-sm toggle-primary"
        aria-label={label}
        checked={value === true}
        {disabled}
        onchange={(event) => onchange(event.currentTarget.checked)}
      />
      <span class="text-sm">{value === true ? "On" : "Off"}</span>
    </label>
  {:else if kind.kind === "choice"}
    {@const other = typeof value === "string" && !kind.symbols.includes(value)}
    <div class="flex flex-wrap items-center gap-2">
      <select
        {id}
        class={[
          "select w-full sm:w-auto sm:min-w-48",
          depth > 0 && "select-sm",
        ]}
        aria-label={label}
        aria-invalid={problem !== null}
        aria-describedby={problem ? problemId : undefined}
        {disabled}
        value={other ? OTHER : typeof value === "string" ? value : ""}
        onchange={(event) => {
          const chosen = event.currentTarget.value;
          onchange(chosen === OTHER ? "" : chosen);
        }}
      >
        {#if value === undefined || value === null}<option value="" disabled
            >{value === null ? "No value" : "Choose…"}</option
          >{/if}
        {#each kind.symbols as symbol (symbol)}<option value={symbol}
            >{symbol}</option
          >{/each}
        <option value={OTHER}>Other value…</option>
      </select>
      {#if other}<input
          class={["input min-w-0 flex-1", size]}
          aria-label={`${label}, other value`}
          autocomplete="off"
          spellcheck="false"
          {disabled}
          value={typeof value === "string" ? value : ""}
          oninput={(event) => onchange(event.currentTarget.value)}
        />{/if}
    </div>
  {:else if kind.kind === "integer" || kind.kind === "number"}
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      <input
        {id}
        type="number"
        class={["input w-full tabular-nums sm:w-48", size]}
        aria-label={label}
        aria-invalid={problem !== null}
        aria-describedby={problem ? problemId : undefined}
        step={kind.kind === "integer" ? 1 : "any"}
        min={kind.min}
        max={kind.max}
        {placeholder}
        {disabled}
        value={typeof value === "number" ? value : ""}
        oninput={(event) => {
          const text = event.currentTarget.value;
          onchange(text === "" ? undefined : event.currentTarget.valueAsNumber);
        }}
      />
      {@render hint(bounds(kind))}
    </div>
  {:else if kind.kind === "timestamp"}
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      <input
        {id}
        type="datetime-local"
        step="1"
        class={["input w-full sm:w-64", size]}
        aria-label={label}
        aria-invalid={problem !== null}
        aria-describedby={problem ? problemId : undefined}
        {disabled}
        value={typeof value === "string" ? toLocalInput(value) : ""}
        oninput={(event) => onchange(fromLocalInput(event.currentTarget.value))}
      />
      {@render hint(
        typeof value === "string"
          ? `Your local time; ${value} in UTC`
          : "Your local time",
      )}
    </div>
  {:else if kind.kind === "json"}
    <textarea
      {id}
      class="textarea w-full font-mono text-sm"
      rows="3"
      aria-label={label}
      aria-invalid={problem !== null}
      aria-describedby={problem ? problemId : undefined}
      spellcheck="false"
      {disabled}
      value={jsonText(value)}
      oninput={(event) => onchange(parseJson(event.currentTarget.value))}
    ></textarea>
    <span class="text-xs text-base-content/55"
      >{kind.reason === "any"
        ? "Edited as JSON: this setting takes any JSON value."
        : "Edited as JSON: this type nests too deeply for a form."}</span
    >
  {:else if kind.kind === "text" || kind.kind === "bigint"}
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      <input
        {id}
        class={[
          "input w-full",
          size,
          kind.kind !== "text" || kind.format !== "text"
            ? "font-mono sm:w-72"
            : "",
        ]}
        inputmode={kind.kind === "bigint" ? "numeric" : undefined}
        aria-label={label}
        aria-invalid={problem !== null}
        aria-describedby={problem ? problemId : undefined}
        autocomplete="off"
        {placeholder}
        {disabled}
        value={typeof value === "string" ? value : ""}
        oninput={(event) => onchange(event.currentTarget.value)}
      />
      {@render hint(bounds(kind))}
    </div>
  {/if}
{/snippet}

{#snippet problemLine()}
  {#if problem}<p id={problemId} class="text-xs text-error">{problem}</p>{/if}
{/snippet}

{#if shape.kind === "nullable" && value === null && inner.kind !== "text"}
  <div class="flex flex-wrap items-center gap-2 py-1">
    <span class="text-sm text-base-content/60">No value</span>
    <button
      type="button"
      class="btn btn-ghost btn-sm"
      aria-label={`Set a value for ${label}`}
      {disabled}
      onclick={() => onchange(emptyValue(schema, defs, depth))}
      >Set a value</button
    >
  </div>
{:else if shape.kind === "nullable" && inner.kind !== "text"}
  <div class="flex items-start gap-2">
    <div class="min-w-0 flex-1">
      <SettingValue
        schema={shape.inner}
        {defs}
        {value}
        {onchange}
        {id}
        {label}
        {disabled}
        {checked}
        {placeholder}
        depth={depth + 1}
      />
    </div>
    {#if clearable}<button
        type="button"
        class="btn btn-ghost btn-sm shrink-0"
        aria-label={`Set ${label} to no value`}
        {disabled}
        onclick={() => onchange(null)}>No value</button
      >{/if}
  </div>
{:else if inner.kind === "list"}
  {@const kind = inner}
  <div class="grid gap-2" role="group" aria-label={label}>
    {#if items.length === 0}<p class="text-sm text-base-content/60">
        No items yet.
      </p>{:else}
      <ol class="grid gap-2">
        {#each items as item, index (index)}
          {@const name = `${label} ${index + 1}`}
          <li class="flex items-start gap-2">
            <span
              class="w-5 shrink-0 pt-2 text-right text-xs text-base-content/50 tabular-nums"
              aria-hidden="true">{index + 1}</span
            >
            <div class="min-w-0 flex-1">
              <SettingValue
                schema={kind.item}
                {defs}
                value={item}
                onchange={(next) => {
                  const updated = [...items];
                  updated[index] = next;
                  onchange(updated);
                }}
                id={`${id}-${index}`}
                label={name}
                {disabled}
                {checked}
                hinted={false}
                depth={depth + 1}
              />
            </div>
            <div class="flex shrink-0 items-center">
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-square"
                aria-label={`Move ${name} up`}
                disabled={disabled || index === 0}
                onclick={() => onchange(moved(index, -1))}
                ><Icon name="chevronUp" size={16} /></button
              >
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-square"
                aria-label={`Move ${name} down`}
                disabled={disabled || index === items.length - 1}
                onclick={() => onchange(moved(index, 1))}
                ><Icon name="chevronDown" size={16} /></button
              >
              <button
                type="button"
                class="btn btn-ghost btn-sm btn-square"
                aria-label={`Remove ${name}`}
                {disabled}
                onclick={() =>
                  onchange(items.filter((_, other) => other !== index))}
                ><Icon name="close" size={16} /></button
              >
            </div>
          </li>
        {/each}
      </ol>
    {/if}
    <div class="flex flex-wrap items-center gap-3">
      <button
        type="button"
        class="btn btn-outline btn-sm"
        aria-label={`Add item to ${label}`}
        disabled={disabled ||
          (kind.maxItems !== undefined && items.length >= kind.maxItems)}
        onclick={() =>
          onchange([...items, emptyValue(kind.item, defs, depth + 1)])}
        ><Icon name="plus" size={14} />Add item</button
      >
      {@render hint(
        [
          kind.minItems !== undefined && kind.maxItems !== undefined
            ? `${kind.minItems} to ${kind.maxItems} items`
            : kind.maxItems !== undefined
              ? `Up to ${kind.maxItems} items`
              : kind.minItems !== undefined
                ? `At least ${kind.minItems} items`
                : "",
          bounds(shapeOf(kind.item, defs, depth + 1)).replace(
            /^./,
            (first) => `Each ${first.toLowerCase()}`,
          ),
        ]
          .filter(Boolean)
          .join("; "),
      )}
    </div>
    {@render problemLine()}
  </div>
{:else if inner.kind === "map"}
  {@const kind = inner}
  {@const entries = Object.entries(record)}
  <div class="grid gap-2" role="group" aria-label={label}>
    {#if entries.length === 0}<p class="text-sm text-base-content/60">
        No entries yet.
      </p>{/if}
    {#each entries as [key, item], index (index)}
      {@const name = `${label} ${key || index + 1}`}
      <div
        class="grid items-start gap-2 sm:grid-cols-[minmax(0,12rem)_minmax(0,1fr)_auto]"
      >
        <div class="grid gap-1">
          <input
            class="input input-sm w-full font-mono"
            aria-label={`Name of ${label} entry ${index + 1}`}
            placeholder="Name"
            autocomplete="off"
            spellcheck="false"
            {disabled}
            value={key}
            oninput={(event) => renamed(index, event.currentTarget.value)}
          />
          {#if clash[index]}<span class="text-xs text-error"
              >Another entry has this name.</span
            >{/if}
        </div>
        <SettingValue
          schema={kind.value}
          {defs}
          value={item}
          onchange={(next) => onchange({ ...record, [key]: next })}
          id={`${id}-${index}`}
          label={name}
          {disabled}
          {checked}
          depth={depth + 1}
        />
        <button
          type="button"
          class="btn btn-ghost btn-sm btn-square"
          aria-label={`Remove ${name}`}
          {disabled}
          onclick={() =>
            onchange(
              Object.fromEntries(entries.filter((_, other) => other !== index)),
            )}><Icon name="close" size={16} /></button
        >
      </div>
    {/each}
    <div>
      <button
        type="button"
        class="btn btn-outline btn-sm"
        aria-label={`Add entry to ${label}`}
        disabled={disabled || "" in record}
        onclick={() =>
          onchange({ ...record, "": emptyValue(kind.value, defs, depth + 1) })}
        ><Icon name="plus" size={14} />Add entry</button
      >
    </div>
    {@render problemLine()}
  </div>
{:else if inner.kind === "model"}
  {@const kind = inner}
  <div
    class="grid gap-3 rounded-box border border-base-300 p-3"
    role="group"
    aria-label={label}
  >
    {#each kind.fields as child (child.name)}
      {@const childId = `${id}-${child.name}`}
      {@const childLabel = `${label} ${fieldLabel(child.name).toLowerCase()}`}
      {@const childValue = fields[child.name]}
      <div class="trellis-field">
        <span class="flex flex-wrap items-center gap-2">
          <label class="trellis-field-label" for={childId}
            >{fieldLabel(child.name)}</label
          >
          {#if !child.required}<span class="text-xs text-base-content/55"
              >Optional</span
            >{/if}
        </span>
        {#if !child.required && childValue === undefined}
          <div>
            <button
              type="button"
              id={childId}
              class="btn btn-ghost btn-sm"
              aria-label={`Add ${childLabel}`}
              {disabled}
              onclick={() =>
                onchange({
                  ...fields,
                  [child.name]:
                    emptyValue(child.schema, defs, depth + 1) ?? null,
                })}><Icon name="plus" size={14} />Add</button
            >
          </div>
        {:else}
          <div class="flex items-start gap-2">
            <div class="min-w-0 flex-1">
              <SettingValue
                schema={child.schema}
                {defs}
                value={childValue}
                onchange={(next) => onchange({ ...fields, [child.name]: next })}
                id={childId}
                label={childLabel}
                {disabled}
                {checked}
                depth={depth + 1}
              />
            </div>
            {#if !child.required}<button
                type="button"
                class="btn btn-ghost btn-sm shrink-0"
                aria-label={`Remove ${childLabel}`}
                {disabled}
                onclick={() =>
                  onchange(
                    Object.fromEntries(
                      Object.entries(fields).filter(
                        ([name]) => name !== child.name,
                      ),
                    ),
                  )}>Remove</button
              >{/if}
          </div>
        {/if}
        {#if child.description}<span class="trellis-field-help"
            >{child.description}</span
          >{/if}
      </div>
    {/each}
    {#if kind.fields.length === 0}<p class="text-sm text-base-content/60">
        This type has no fields.
      </p>{/if}
    {@render problemLine()}
  </div>
{:else}
  <div class="grid gap-1">
    {@render leaf(inner)}
    {#if shape.kind === "nullable" && clearable && value !== null}<div>
        <button
          type="button"
          class="btn btn-ghost btn-xs"
          aria-label={`Set ${label} to no value`}
          {disabled}
          onclick={() => onchange(null)}>No value</button
        >
      </div>{/if}
    {@render problemLine()}
  </div>
{/if}
