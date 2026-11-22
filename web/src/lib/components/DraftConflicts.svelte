<script lang="ts">
  /** A field someone else saved while it was being edited here. */
  type Conflict = {
    field: string;
    label: string;
    /** What was typed here, as shown to the operator. */
    mine: string;
    /** What is saved now, as shown to the operator. */
    saved: string;
  };

  let {
    conflicts,
    disabled = false,
    onsettle,
  }: {
    conflicts: readonly Conflict[];
    disabled?: boolean;
    /** Keep the typed value, or take the saved one, for one field. */
    onsettle: (field: string, choice: "keep" | "take") => void;
  } = $props();
</script>

{#if conflicts.length > 0}
  <div
    class="rounded-box border border-warning/60 bg-warning/10 p-4 text-sm"
    role="group"
    aria-label="Changed while you were editing"
  >
    <p class="font-medium">
      Someone saved {conflicts.length === 1 ? "this" : "these"} while you were editing.
      Choose which to keep before saving.
    </p>
    <ul class="mt-3 divide-y divide-base-300">
      {#each conflicts as conflict (conflict.field)}
        <li
          class="grid gap-2 py-2 sm:grid-cols-[1fr_auto] sm:items-center"
          aria-label={`${conflict.label} changed meanwhile`}
        >
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            <dt class="col-span-2 font-medium">{conflict.label}</dt>
            <dt class="text-base-content/65">Yours</dt>
            <dd class="break-all" data-draft="mine">{conflict.mine}</dd>
            <dt class="text-base-content/65">Saved</dt>
            <dd class="break-all" data-draft="saved">{conflict.saved}</dd>
          </dl>
          <div class="flex gap-2">
            <button
              type="button"
              class="btn btn-sm btn-outline"
              {disabled}
              aria-label={`Keep yours for ${conflict.label}`}
              onclick={() => onsettle(conflict.field, "keep")}
              >Keep yours</button
            ><button
              type="button"
              class="btn btn-sm btn-ghost"
              {disabled}
              aria-label={`Use saved for ${conflict.label}`}
              onclick={() => onsettle(conflict.field, "take")}>Use saved</button
            >
          </div>
        </li>
      {/each}
    </ul>
  </div>
{/if}
