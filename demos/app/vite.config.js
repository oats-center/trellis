import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";

const config = {
  plugins: [tailwindcss(), sveltekit()],
  resolve: {
    dedupe: ["svelte"],
  },
};

export default config;
