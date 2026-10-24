// Sveltepress gives headings IDs like "Per-message-proof-digests"; the guides and
// design docs link to GitHub-style IDs like "per-message-proof-digests". This adds
// the GitHub-style ID beside the theme's so both forms of link work.
type Node = {
  type: string;
  value?: string;
  children?: Node[];
  data?: { hName?: string; hProperties?: Record<string, unknown> };
};

function text(node: Node): string {
  if (node.type === "Anchor") return "";
  if (typeof node.value === "string") return node.value;
  return (node.children ?? []).map(text).join("");
}

/** GitHub's heading slug: lower case, punctuation dropped, spaces to hyphens. */
export function githubSlug(title: string): string {
  return title
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, "")
    .replace(/\s/g, "-");
}

export function githubAnchors() {
  return (tree: Node) => {
    const seen = new Map<string, number>();
    const walk = (parent: Node) => {
      const children = parent.children ?? [];
      for (let index = 0; index < children.length; index++) {
        const node = children[index];
        const next = children[index + 1];
        if (node.type === "AnchorId" && next?.type === "heading") {
          const base = githubSlug(text(next));
          const count = seen.get(base) ?? 0;
          seen.set(base, count + 1);
          const id = count ? `${base}-${count}` : base;
          if (id && id !== node.data?.hProperties?.id) {
            children.splice(index + 1, 0, {
              type: "AnchorId",
              data: { hName: "div", hProperties: { id, className: "svp-anchor-item" } },
            });
            index++;
          }
        } else if (node.children) {
          walk(node);
        }
      }
    };
    walk(tree);
  };
}
