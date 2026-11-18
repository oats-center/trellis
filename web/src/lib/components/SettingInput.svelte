<script lang="ts">
  import type { Snippet } from "svelte";
  import Icon from "$lib/components/Icon.svelte";
  import {
    isTextSetting,
    type SettingDeclaration,
  } from "$lib/console/settings.ts";

  // One setting a build declares: its label and marks, its input, and its
  // description. Secrets are typed hidden; only what was typed can be shown.
  type Props = {
    field: SettingDeclaration;
    id: string;
    /** What the input shows: a draft, or a setting's saved value. */
    value: string;
    placeholder: string;
    disabled: boolean;
    /** The Required mark warns: nothing usable is set. */
    missing?: boolean;
    /** The typed secret is shown instead of hidden. */
    revealed?: boolean;
    oninput: (text: string) => void;
    onreveal?: () => void;
    /** Where the value comes from, beside the marks. */
    status?: Snippet;
    /** Buttons after the input. */
    actions?: Snippet;
    /** A line under the description. */
    note?: Snippet;
  };
  let {
    field,
    id,
    value,
    placeholder,
    disabled,
    missing = false,
    revealed = false,
    oninput,
    onreveal,
    status,
    actions,
    note,
  }: Props = $props();
</script>

<div class="trellis-field">
  <span class="flex flex-wrap items-center gap-2">
    <label class="trellis-field-label" for={id}>{field.title}</label>
    {#if field.required}<span
        class={["badge badge-sm", missing ? "badge-warning" : "badge-ghost"]}
        >Required</span
      >{/if}
    {#if field.secret}<span
        class="inline-flex items-center gap-1 text-xs text-base-content/60"
        ><Icon name="lock" size={12} />Secret</span
      >{/if}
    {#if field.restart}<span class="badge badge-neutral badge-sm"
        >Applies after restart</span
      >{/if}
    {@render status?.()}
  </span>
  <span class="flex gap-2">
    <label
      class={[
        "input w-full",
        !isTextSetting(field) && "font-mono",
        disabled && "input-disabled",
      ]}
      ><input
        {id}
        class="grow"
        type={field.secret && !(revealed && value) ? "password" : "text"}
        autocomplete="off"
        {disabled}
        {placeholder}
        {value}
        oninput={(event) => oninput(event.currentTarget.value)}
      />{#if field.secret && onreveal}<button
          type="button"
          class="btn btn-ghost btn-xs btn-square -mr-2"
          aria-label={`${revealed ? "Hide" : "Show"} ${field.title}`}
          aria-pressed={revealed}
          disabled={!value}
          title={value
            ? undefined
            : "A saved secret is never shown; type a new one to see it."}
          onclick={onreveal}
          ><Icon name={revealed ? "eyeOff" : "eye"} size={16} /></button
        >{/if}</label
    >
    {@render actions?.()}
  </span>
  <span class="trellis-field-help">{field.description}</span>
  {@render note?.()}
</div>
