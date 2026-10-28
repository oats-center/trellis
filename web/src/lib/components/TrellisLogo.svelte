<script lang="ts">
  import type { ClassValue } from "svelte/elements";
  import TrellisMark from "./TrellisMark.svelte";

  type Props = {
    class?: ClassValue;
    size?: number;
  };

  let { class: className, size = 30 }: Props = $props();
</script>

<span class={["inline-flex items-center gap-2.5", className]}>
  <TrellisMark {size} />
  <span class="text-[1.75rem] font-semibold leading-none tracking-[-0.03em]"
    >Trellis</span
  >
</span>
