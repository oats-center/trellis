// Publishes the repository's design/ Markdown as docs pages under /design/.
// The design tree stays the source of truth; the generated routes and sidebar
// are build output and are not committed.
import {
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, posix, relative } from "node:path";
import { fileURLToPath } from "node:url";

const docsRoot = fileURLToPath(new URL("..", import.meta.url));
const designRoot = join(docsRoot, "..", "design");
const routesRoot = join(docsRoot, "src", "routes", "design");
const sidebarPath = join(docsRoot, "design-sidebar.json");
const repoBlob = "https://github.com/oats-center/trellis/blob/main";

/** Topic folders in reading order, with the names shown in the sidebar. */
const topics: Array<[string, string]> = [
  ["core", "Core"],
  ["auth", "Auth"],
  ["contracts", "Contracts"],
  ["operations", "Operations"],
  ["jobs", "Jobs"],
  ["tooling", "Tooling"],
];

type Doc = { file: string; slug: string; title: string; body: string };

function markdownFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return markdownFiles(path);
    return entry.isFile() && entry.name.endsWith(".md") ? [path] : [];
  });
}

/** "core/trellis-patterns.md" → "core/trellis-patterns"; READMEs name their folder. */
function slugOf(file: string): string {
  return relative(designRoot, file)
    .replaceAll("\\", "/")
    .replace(/\.md$/, "")
    .replace(/(^|\/)README$/i, "");
}

function routeOf(slug: string): string {
  return slug ? `/design/${slug}` : "/design";
}

function frontmatter(source: string): {
  data: Record<string, string>;
  body: string;
} {
  const match = source.match(/^---\n([\s\S]*?)\n---\n?/);
  if (!match) return { data: {}, body: source };
  const data: Record<string, string> = {};
  for (const line of match[1].split("\n")) {
    const field = line.match(/^(\w+):\s*(.*)$/);
    if (field) data[field[1]] = field[2].replace(/^["']|["']$/g, "");
  }
  return { data, body: source.slice(match[0].length) };
}

/** Links to other design docs become routes; links into the repo go to GitHub. */
function rewriteLinks(body: string, file: string): string {
  const from = posix.dirname(relative(designRoot, file).replaceAll("\\", "/"));
  return body.replace(
    /\]\((?!https?:|mailto:|#|\/)([^)\s]+?)(#[^)\s]*)?\)/g,
    (whole, target: string, hash = "") => {
      const resolved = posix.normalize(posix.join(from, target));
      if (resolved.endsWith(".md") && !resolved.startsWith("..")) {
        const slug = resolved.replace(/\.md$/, "").replace(/(^|\/)README$/i, "");
        return `](${routeOf(slug)}${hash})`;
      }
      const repoPath = posix.normalize(posix.join("design", from, target));
      return `](${repoBlob}/${repoPath}${hash})`;
    },
  );
}

const docs: Doc[] = markdownFiles(designRoot).map((file) => {
  const { data, body } = frontmatter(readFileSync(file, "utf8"));
  const heading = body.match(/^#\s+(.+)$/m)?.[1]?.trim();
  const title = data.title ?? heading ?? slugOf(file);
  // The page title renders from frontmatter, so drop a duplicate first heading.
  const content = body.replace(/^\s*#\s+.+\n/, "");
  const description = data.description
    ? `description: ${JSON.stringify(data.description)}\n`
    : "";
  return {
    file,
    slug: slugOf(file),
    title,
    body: `---\ntitle: ${JSON.stringify(title)}\n${description}---\n\n${rewriteLinks(content, file).trimStart()}`,
  };
});

if (existsSync(routesRoot)) rmSync(routesRoot, { recursive: true });
for (const doc of docs) {
  const target = join(routesRoot, doc.slug, "+page.md");
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, doc.body);
}

const sidebar = [
  {
    title: "Design",
    collapsible: false,
    items: [{ title: "Index", to: "/design" }],
  },
  ...topics
    .map(([folder, label]) => ({
      title: label,
      collapsible: true,
      items: docs
        .filter((doc) => doc.slug.startsWith(`${folder}/`) || doc.slug === folder)
        .sort((left, right) =>
          left.title.replace(/^Design:\s*/, "").localeCompare(right.title.replace(/^Design:\s*/, ""))
        )
        .map((doc) => ({
          title: doc.title.replace(/^Design:\s*/, ""),
          to: routeOf(doc.slug),
        })),
    }))
    .filter((group) => group.items.length > 0),
];
writeFileSync(sidebarPath, `${JSON.stringify(sidebar, null, 2)}\n`);
console.log(`Published ${docs.length} design docs.`);
