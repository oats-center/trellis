<script lang="ts">
  type Tone = "success" | "warning" | "error" | "info" | "neutral";

  type Item = {
    id: string;
    label: string;
    value: string | number;
    detail?: string;
    tone?: Tone;
    active?: boolean;
    attention?: boolean;
    disabled?: boolean;
  };

  type Props = {
    items: Item[];
    ariaLabel: string;
    onSelect: (id: string) => void;
    class?: string;
  };

  let { items, ariaLabel, onSelect, class: className = "" }: Props = $props();
</script>

<div class={["trellis-ledger", className]} aria-label={ariaLabel}>
  {#each items as item (item.id)}
    <button
      type="button"
      class:active={item.active}
      class:attention={item.attention}
      aria-pressed={item.active ?? false}
      disabled={item.disabled}
      onclick={() => onSelect(item.id)}
    >
      <span>
        {#if item.tone}<i class={["trellis-dot", item.tone]}></i>{/if}
        {item.label}
      </span>
      <strong>{item.value}</strong>
      {#if item.detail}<small>{item.detail}</small>{/if}
    </button>
  {/each}
</div>
