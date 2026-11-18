<script lang="ts">
  import type { FreeSetting } from "$lib/console/settings.ts";

  // Settings named by hand, for fields no build Trellis knows declares yet:
  // a service added without its IDL, or values for its next version. Each row
  // is a name and a value, typed as text or JSON, optionally kept secret.
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

<div class="space-y-2">
  {#each rows as row, index (index)}
    {@const number = index + 1}
    <div
      class="grid gap-2 rounded-box border border-base-300 p-3 sm:grid-cols-[minmax(0,1fr)_7rem_minmax(0,1.5fr)_auto]"
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
        ><option value="text">Text</option><option value="json">JSON</option
        ></select
      >
      <input
        class={["input input-sm w-full", row.format === "json" && "font-mono"]}
        aria-label={`Value of setting ${number}`}
        type={row.secret ? "password" : "text"}
        placeholder={row.format === "json" ? "9090, true, or [1, 2]" : "Value"}
        autocomplete="off"
        spellcheck="false"
        {disabled}
        bind:value={row.value}
      />
      <span class="flex items-center gap-2">
        {#if allowSecrets}<label
            class="flex items-center gap-1.5 text-sm whitespace-nowrap"
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
          class="btn btn-ghost btn-sm"
          aria-label={`Remove setting ${number}`}
          {disabled}
          onclick={() => {
            rows = rows.filter((_, other) => other !== index);
          }}>Remove</button
        >
      </span>
    </div>
  {/each}
  <button
    type="button"
    class="btn btn-outline btn-sm"
    {disabled}
    onclick={() => {
      rows = [...rows, { name: "", format: "text", secret: false, value: "" }];
    }}>Add a setting</button
  >
</div>
