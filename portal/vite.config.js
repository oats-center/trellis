import { svelte } from "@sveltejs/vite-plugin-svelte";

export default {
  base: "/portal-assets/",
  plugins: [svelte()],
  build: { manifest: true },
  resolve: { dedupe: ["svelte"] },
};
