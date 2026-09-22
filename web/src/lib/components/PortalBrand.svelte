<script lang="ts">
  import logoUrl from "../../../../art/trellis-logo.svg";

  interface Props {
    subtitle: string;
  }

  let { subtitle }: Props = $props();
</script>

<div class="inline-flex items-center gap-5">
  <span
    class="portal-brand-mark grid h-20 w-20 shrink-0 place-items-center"
  >
    <span
      class="trellis-logo-mask h-18 w-18"
      style:--trellis-logo-url={`url("${logoUrl}")`}
      aria-hidden="true"
    ></span>
  </span>
  <span class="min-w-0">
    <span class="block text-[2.45rem] font-bold leading-[0.88] tracking-[-0.055em] text-base-content">
      Trellis
    </span>
    <span class="portal-eyebrow mt-2 block text-[0.7rem] font-semibold sm:text-[0.78rem]">
      {subtitle}
    </span>
  </span>
</div>
