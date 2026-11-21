<script lang="ts">
  import Icon from "$lib/components/Icon.svelte";
  import type { FreeSetting } from "$lib/console/settings.ts";

  // Settings named by hand, for fields no build Trellis knows declares yet:
  // a service added without its IDL, or values for its next version. Each row
  // is a name and a value of the kind chosen for it (text, a number, on or
  // off, or JSON for lists and objects), optionally kept secret.
  type Props = {
    rows: FreeSetting[];
    /** Apps have no secrets: every user of an app reads its settings. */
    allowSecrets?: boolean;
    disabled?: boolean;
  };
  let {
    rows = $bindable([]),
    allowSecrets = true,
    disabled = false,
  }: Props = $props();
</script>

<div class="grid gap-2">
  {#each rows as row, index (index)}
    {@const number = index + 1}
    <div
      class="grid items-start gap-2 rounded-box border border-base-300 p-3 sm:grid-cols-[minmax(0,1fr)_8.5rem_minmax(0,1.5fr)_auto]"
    >
      <input
        class="input input-sm w-full font-mono"
        aria-label={`Name of setting ${number}`}
        placeholder="Name, such as databaseUrl"
        autocomplete="off"
        spellcheck="false"
        {disabled}
        bind:value={row.name}
      />
      <select
        class="select select-sm w-full"
        aria-label={`Format of setting ${number}`}
        {disabled}
        bind:value={row.format}
        onchange={(event) => {
          if (event.currentTarget.value === "boolean") row.value = "false";
        }}
        ><option value="text">Text</option><option value="number">Number</option
        ><option value="boolean">On or off</option><option value="json"
          >JSON</option
        ></select
      >
      {#if row.format === "boolean"}
        <label class="flex h-8 w-fit cursor-pointer items-center gap-2">
          <input
            type="checkbox"
            class="toggle toggle-sm toggle-primary"
            aria-label={`Value of setting ${number}`}
            {disabled}
            checked={row.value === "true"}
            onchange={(event) => {
              row.value = event.currentTarget.checked ? "true" : "false";
            }}
          />
          <span class="text-sm">{row.value === "true" ? "On" : "Off"}</span>
        </label>
      {:else if row.format === "json" && !row.secret}
        <textarea
          class="textarea textarea-sm w-full font-mono"
          rows="2"
          aria-label={`Value of setting ${number}`}
          placeholder={'["a", "b"] or {"retries": 3}'}
          spellcheck="false"
          {disabled}
          bind:value={row.value}
        ></textarea>
      {:else}
        <input
          class={[
            "input input-sm w-full",
            row.format !== "text" && "font-mono",
          ]}
          aria-label={`Value of setting ${number}`}
          type={row.secret
            ? "password"
            : row.format === "number"
              ? "number"
              : "text"}
          step={row.format === "number" ? "any" : undefined}
          placeholder={row.format === "json"
            ? '["a", "b"] or {"retries": 3}'
            : row.format === "number"
              ? "9090"
              : "Value"}
          autocomplete="off"
          spellcheck="false"
          {disabled}
          value={row.value}
          oninput={(event) => {
            row.value = event.currentTarget.value;
          }}
        />
      {/if}
      <span class="flex items-center gap-2">
        {#if allowSecrets}<label
            class="flex h-8 items-center gap-1.5 text-sm whitespace-nowrap"
            ><input
              type="checkbox"
              class="checkbox checkbox-sm"
              aria-label={`Setting ${number} is a secret`}
              {disabled}
              bind:checked={row.secret}
            />Secret</label
          >{/if}
        <button
          type="button"
          class="btn btn-ghost btn-sm btn-square"
          aria-label={`Remove setting ${number}`}
          {disabled}
          onclick={() => {
            rows = rows.filter((_, other) => other !== index);
          }}><Icon name="close" size={16} /></button
        >
      </span>
    </div>
  {/each}
  <div>
    <button
      type="button"
      class="btn btn-outline btn-sm"
      {disabled}
      onclick={() => {
        rows = [
          ...rows,
          { name: "", format: "text", secret: false, value: "" },
        ];
      }}><Icon name="plus" size={14} />Add a setting</button
    >
  </div>
</div>
