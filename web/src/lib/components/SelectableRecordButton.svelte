<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes, ClassValue } from "svelte/elements";

  type ButtonTone = "primary" | "error";

  type Props = {
    selected: boolean;
    children: Snippet;
    onclick?: HTMLButtonAttributes["onclick"];
    tone?: ButtonTone;
    class?: ClassValue;
    selectedClass?: ClassValue;
    unselectedClass?: ClassValue;
  };

  let {
    selected,
    children,
    onclick,
    tone = "primary",
    class: className,
    selectedClass,
    unselectedClass = "border-base-300 bg-base-100 hover:border-base-content/20",
  }: Props = $props();

  let defaultSelectedClass = $derived(tone === "error" ? "border-error bg-error/10" : "border-primary bg-primary/5");
</script>

<button
  type="button"
  class={[
    "w-full min-w-0 rounded-box border p-3 text-left transition-colors",
    selected ? (selectedClass ?? defaultSelectedClass) : unselectedClass,
    className,
  ]}
  {onclick}
>
  {@render children()}
</button>
