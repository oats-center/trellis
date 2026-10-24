<script lang="ts">
  let { size = 28 }: { size?: number } = $props();
</script>

<!-- Same artwork as art/trellis-mark.svg; inlined because the portal CSP allows no images. -->
<svg
  width={size}
  height={size * (420 / 330)}
  viewBox="0 0 330 420"
  aria-hidden="true"
>
  <path
    fill="#6b6540"
    d="M3 145h54c13 0 25 5 35 15l63 66c11 12 17 24 17 40v149h-32V290c0-25-18-41-42-41H70c-37 0-67-27-67-64z"
  />
  <path
    fill="#8e7f47"
    d="M233 2v83c0 45-38 80-75 115-33-35-58-60-54-95 4-45 56-80 129-103z"
  />
  <path
    fill="#c2a86e"
    d="M325 142v28c0 55-40 97-93 97h-50v-35c0-52 38-90 93-90z"
  />
  <path
    fill="#ccc7b0"
    d="M3 268h72c30 0 53 24 53 54v48H78c-42 0-75-33-75-70z"
  />
</svg>
