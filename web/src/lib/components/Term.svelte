<script lang="ts">
  import { GLOSSARY } from "../glossary.ts";

  type Props = {
    term: string;
    class?: string;
  };

  let { term, class: className = "" }: Props = $props();

  const definition = $derived(GLOSSARY[term]);
  const slug = $derived(term.toLowerCase().replace(/[^a-z0-9]+/g, "-"));
</script>

{#if definition}
  <button
    type="button"
    class={["trellis-term", className]}
    aria-describedby={`term-${slug}`}
    popovertarget={`term-${slug}`}
  >{term}</button>
  <div id={`term-${slug}`} class="trellis-term-popover" popover="auto">
    <p>{definition}</p>
  </div>
{:else}
  <span class={className}>{term}</span>
{/if}
