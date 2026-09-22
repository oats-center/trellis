import type { NavSection } from "./control-panel.ts";

/** A single ⌘K navigation target: a page or a preset filtered view. */
export type CommandEntry = {
  id: string;
  label: string;
  group: string;
  href: string;
  query?: string;
  keywords: string;
  breadcrumb: string;
};

const presetViews: CommandEntry[] = [
  {
    id: "view:jobs:action",
    label: "Jobs: action needed",
    group: "Views",
    href: "/admin/jobs",
    query: "focus=action",
    keywords: "jobs failed dead retry dlq dead-letter problems",
    breadcrumb: "Jobs",
  },
  {
    id: "view:jobs:running",
    label: "Jobs: running",
    group: "Views",
    href: "/admin/jobs",
    query: "focus=running",
    keywords: "jobs active in progress workers",
    breadcrumb: "Jobs",
  },
  {
    id: "view:jobs:backlog",
    label: "Jobs: backlog",
    group: "Views",
    href: "/admin/jobs",
    query: "focus=backlog",
    keywords: "jobs pending queued retrying backlog",
    breadcrumb: "Jobs",
  },
  {
    id: "view:jobs:dead",
    label: "Jobs: dead-lettered",
    group: "Views",
    href: "/admin/jobs",
    query: "focus=dead",
    keywords: "jobs dlq dead-letter dead queue",
    breadcrumb: "Jobs",
  },
  {
    id: "view:events:exceptions",
    label: "Events: integrity exceptions",
    group: "Views",
    href: "/admin/events",
    query: "focus=exceptions",
    keywords: "events verification unproven auth-failure tampered integrity",
    breadcrumb: "Events",
  },
  {
    id: "view:events:unresolved",
    label: "Events: unresolved owners",
    group: "Views",
    href: "/admin/events",
    query: "focus=unresolved",
    keywords: "events unresolved owner attribution",
    breadcrumb: "Events",
  },
  {
    id: "view:sessions:connections",
    label: "Sessions: connections",
    group: "Views",
    href: "/admin/sessions",
    query: "tab=connections",
    keywords: "sessions connections live nats clients",
    breadcrumb: "Sessions",
  },
];

/**
 * Builds the navigate-only command index from the visible nav sections plus
 * preset operator views. Palette is navigation only; it never mutates state.
 */
export function buildCommandIndex(navSections: NavSection[]): CommandEntry[] {
  const pages: CommandEntry[] = navSections.flatMap((section) =>
    section.items.map((item) => ({
      id: `page:${item.href}`,
      label: item.label,
      group: "Pages",
      href: item.href,
      keywords: item.label.toLowerCase(),
      breadcrumb: section.title,
    }))
  );
  const byHref = new Map<string, CommandEntry>();
  for (const entry of [...pages, ...presetViews]) {
    const dedupeKey = `${entry.href}?${entry.query ?? ""}`;
    if (!byHref.has(dedupeKey)) byHref.set(dedupeKey, entry);
  }
  return [...byHref.values()];
}

/**
 * Subsequence fuzzy match, Linear/fzf style: contiguous runs score highest,
 * then early matches, then short labels. Returns entries sorted best-first.
 */
export function filterCommands(
  commands: CommandEntry[],
  query: string,
): CommandEntry[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return commands;
  const scored: { entry: CommandEntry; score: number }[] = [];
  for (const entry of commands) {
    const haystack = `${entry.label} ${entry.keywords}`.toLowerCase();
    const score = scoreMatch(needle, haystack);
    if (score > 0) scored.push({ entry, score });
  }
  scored.sort((a, b) =>
    b.score - a.score || a.entry.label.length - b.entry.label.length
  );
  return scored.map((item) => item.entry);
}

function scoreMatch(needle: string, haystack: string): number {
  let score = 0;
  let cursor = 0;
  let runStreak = 0;
  for (const ch of needle) {
    const at = haystack.indexOf(ch, cursor);
    if (at === -1) return 0;
    if (at === cursor) {
      runStreak += 1;
      score += 4 * runStreak;
    } else {
      runStreak = 0;
      score += Math.max(1, 8 - (at - cursor));
    }
    if (at === 0 || haystack[at - 1] === " ") score += 6;
    cursor = at + 1;
  }
  return score + 20 - Math.min(20, haystack.length / 12);
}
