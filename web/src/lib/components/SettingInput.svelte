<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "$lib/components/Icon.svelte";
  import SettingValue from "$lib/components/SettingValue.svelte";
  import {
    formatValue,
    settingSchema,
    type Draft,
    type SettingDeclaration,
  } from "$lib/console/settings.ts";

  // One setting a build declares, as a row: what it is on the left (title,
  // marks, description, default), and on the right an input that fits its
  // type. A secret is a masked field; its eye shows what was typed, or asks
  // Trellis for the saved value when `onreveal` can.
  type Props = {
    field: SettingDeclaration;
    id: string;
    /** The draft, or the saved value while the setting is left alone. */
    value: Draft;
    /** The setting is being edited, so problems with it show. */
    edited?: boolean;
    placeholder?: string;
    disabled: boolean;
    /** The Required mark warns: nothing usable is set. */
    missing?: boolean;
    onchange: (value: Draft) => void;
    /** A secret is saved, so its masked field shows that it is set. */
    secretSet?: boolean;
    /** The secret's text is shown rather than masked. */
    secretShown?: boolean;
    /** Trellis is fetching the saved secret. */
    revealing?: boolean;
    /** Shows or hides the secret; absent where nothing can be shown. */
    onreveal?: () => void;
    /** Why the eye is off, when it is. */
    revealBlocked?: string;
    /** Where the value comes from. */
    status?: Snippet;
    /** Buttons after the input. */
    actions?: Snippet;
    /** A line under the input. */
    note?: Snippet;
  };
  let {
    field,
    id,
    value,
    edited = false,
    placeholder = "",
    disabled,
    missing = false,
    onchange,
    secretSet = false,
    secretShown = false,
    revealing = false,
    onreveal,
    revealBlocked,
    status,
    actions,
    note,
  }: Props = $props();

  const schema = $derived(settingSchema(field));
  const secretText = $derived(typeof value === "string" ? value : "");
  const canToggle = $derived(
    !!onreveal && (secretShown || secretText !== "" || !revealBlocked),
  );
</script>

<div
  class="grid gap-x-8 gap-y-3 py-5 first:pt-1 md:grid-cols-[minmax(0,15rem)_minmax(0,1fr)]"
>
  <div class="grid content-start gap-1.5">
    <label class="text-sm font-semibold" for={id}>{field.title}</label>
    {#if field.required || field.secret || field.restart}<span
        class="flex flex-wrap items-center gap-1.5"
      >
        {#if field.required}<span
            class={[
              "badge badge-sm",
              missing ? "badge-warning" : "badge-ghost",
            ]}>Required</span
          >{/if}
        {#if field.secret}<span class="badge badge-ghost badge-sm gap-1"
            ><Icon name="lock" size={11} />Secret</span
          >{/if}
        {#if field.restart}<span class="badge badge-ghost badge-sm"
            >Applies after restart</span
          >{/if}
      </span>{/if}
    <span class="text-sm text-base-content/65">{field.description}</span>
  </div>
  <div class="grid min-w-0 content-start gap-1.5">
    <div class="flex min-w-0 items-start gap-2">
      <div class="min-w-0 flex-1">
        {#if field.secret}
          <label class={["input w-full", disabled && "input-disabled"]}
            ><input
              {id}
              class="grow font-mono"
              type={secretShown && secretText ? "text" : "password"}
              autocomplete="off"
              spellcheck="false"
              {disabled}
              placeholder={secretSet ? "••••••••" : "Not set"}
              value={secretText}
              oninput={(event) => onchange(event.currentTarget.value)}
            />{#if onreveal}{#if revealing}<span
                  class="loading loading-spinner loading-xs"
                  aria-label={`Getting ${field.title}`}
                ></span>{:else}<button
                  type="button"
                  class="btn btn-ghost btn-xs btn-square -mr-2"
                  aria-label={`${secretShown ? "Hide" : "Show"} ${field.title}`}
                  aria-pressed={secretShown}
                  disabled={disabled || !canToggle}
                  title={canToggle ? undefined : revealBlocked}
                  onclick={onreveal}
                  ><Icon
                    name={secretShown ? "eyeOff" : "eye"}
                    size={16}
                  /></button
                >{/if}{/if}</label
          >
        {:else}
          <SettingValue
            schema={schema.root}
            defs={schema.defs}
            {value}
            {onchange}
            {id}
            label={field.title}
            {disabled}
            checked={edited}
            clearable={false}
            {placeholder}
          />
        {/if}
      </div>
      {#if actions}<div class="flex shrink-0 items-center gap-1">
          {@render actions()}
        </div>{/if}
    </div>
    {#if status || (field.default !== undefined && !field.secret)}<span
        class="flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-base-content/60"
      >
        {@render status?.()}
        {#if field.default === null && !field.secret}<span
            >Default: no value</span
          >{:else if field.default !== undefined && !field.secret}<span
            >Default: <span class="font-mono">{formatValue(field.default)}</span
            ></span
          >{/if}
      </span>{/if}
    {@render note?.()}
  </div>
</div>
