import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";
import { defaultTheme } from "@sveltepress/theme-default";
import { sveltepress } from "@sveltepress/vite";
import { defineConfig } from "vite";
import guides from "./sidebar.json" with { type: "json" };
import { githubAnchors } from "./scripts/github_anchors.ts";
import { trellisGrammar } from "./src/lib/trellis-grammar.ts";
import design from "./design-sidebar.json" with { type: "json" };

// GitHub Pages serves the site from /<repository>; the Pages workflow sets
// SITE_BASE_PATH for the released and current builds.
const githubRepository = process.env.GITHUB_REPOSITORY;
const base = (process.env.SITE_BASE_PATH ??
  (githubRepository ? `/${githubRepository.split("/")[1]}` : "")) as
    | ""
    | `/${string}`;

export default defineConfig({
  plugins: [
    sveltepress({
      theme: defaultTheme({
        logo: "/logo.svg",
        github: "https://github.com/oats-center/trellis",
        editLink:
          "https://github.com/oats-center/trellis/edit/main/docs/src/routes/:route",
        navbar: [
          { title: "Guides", to: "/guides/overview" },
          { title: "Concepts", to: "/guides/concepts" },
          { title: "Design", to: "/design" },
          { title: "API", to: "/api" },
        ],
        sidebar: { "/guides/": guides, "/design": design },
        themeColor: {
          light: "#fbfaf5",
          dark: "#171512",
          primary: "#8a7550",
          primaryDeep: "#6d5c3e",
          hover: "#6b6540",
          gradient: { start: "#6b6540", end: "#c2a86e" },
        },
        highlighter: {
          languages: [
            "bash",
            "sh",
            "shell",
            "console",
            "toml",
            "yaml",
            "json",
            "rust",
            "ts",
            "typescript",
            "js",
            "svelte",
            "html",
            "css",
            "md",
            "sql",
            "mermaid",
            // Shiki accepts a custom grammar object; the theme types only name bundled ones.
            trellisGrammar as unknown as "sql",
          ],
          themeLight: "github-light",
          themeDark: "github-dark-dimmed",
        },
        preBuildIconifyIcons: {
          tabler: [
            "file-code",
            "plug-connected",
            "shield-lock",
            "bolt",
            "list-check",
            "devices",
            "arrow-right",
            "server",
            "browser",
            "cpu",
            "terminal-2",
          ],
        },
      }),
      remarkPlugins: [githubAnchors],
      siteConfig: {
        title: "Trellis",
        description:
          "Trellis connects services, apps, and devices through typed APIs with built-in permissions, events, and background jobs.",
      },
      svelteKitOptions: {
        preprocess: [vitePreprocess()],
        adapter: adapter({ pages: "build", assets: "build" }),
        paths: { base, relative: false },
        prerender: {
          // Generated API reference pages are static files, not routes.
          handleHttpError: ({ path, message }) => {
            if (path.startsWith(`${base}/api/`)) return;
            throw new Error(message);
          },
        },
      },
    }),
  ],
});
