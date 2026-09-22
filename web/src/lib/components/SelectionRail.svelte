<script lang="ts">
  import type { Snippet } from "svelte";
  import Panel from "./Panel.svelte";

  type Props = {
    title: string;
    eyebrow: string;
    children: Snippet;
    footer?: Snippet;
    actions?: Snippet;
    class?: string;
  };

  let { title, eyebrow, children, footer, actions, class: className }: Props = $props();
</script>

<Panel {title} {eyebrow} {actions} {footer} class={`flex min-w-0 flex-col xl:h-full [&>.trellis-section-body]:flex-1 ${className ?? ""}`}>
  {@render children()}
</Panel>
