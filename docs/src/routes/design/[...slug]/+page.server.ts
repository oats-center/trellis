import { error } from "@sveltejs/kit";

const designModules = import.meta.glob("$design/**/*.md");

const designSlugs = Array.from(
  new Set(
    Object.keys(designModules)
      .map((path) =>
        path.replace(/^\$design\//, "").replace(/^.*\/design\//, "").replace(
          /\\/g,
          "/",
        ).replace(/\.md$/, "")
      )
      .map((path) => path.replace(/(?:^|\/)README$/i, ""))
      .filter((slug) => slug.length > 0),
  ),
);

export const prerender = true;

export function entries() {
  return designSlugs.flatMap((slug) => [{ slug }, { slug: `${slug}.md` }]);
}

function normalizeSlug(slug: string) {
  return slug
    .replace(/\/+$/, "")
    .replace(/\.md$/i, "")
    .replace(/(?:^|\/)README$/i, "");
}

export function load({ params }: { params: { slug: string } }) {
  if (!designSlugs.includes(normalizeSlug(params.slug))) {
    throw error(404, "Design document not found");
  }

  return {};
}
