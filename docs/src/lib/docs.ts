export interface DocEntry {
  title: string;
  description: string;
  href: string;
  section: string;
  showPageHeader?: boolean;
  sidebarGroup?: string;
  sidebarLabel?: string;
}

export interface SidebarDocEntry {
  kind: "doc";
  doc: DocEntry;
}

export interface SidebarGroupEntry {
  kind: "group";
  label: string;
  parent: DocEntry;
  docs: DocEntry[];
}

export type SidebarItem = SidebarDocEntry | SidebarGroupEntry;

export interface GuideSidebarSection {
  section: string;
  items: SidebarItem[];
}

const designModules = import.meta.glob("$design/**/*.md");
const designRawModules = import.meta.glob("$design/**/*.md", {
  eager: true,
  query: "?raw",
  import: "default",
}) as Record<string, string>;

export interface DesignGroup {
  slug: string;
  title: string;
  section: string;
  description: string;
  href: string;
}

const designGroupOrder = [
  "Design Overview",
  "Design: Core",
  "Design: Auth",
  "Design: Contracts",
  "Design: Operations",
  "Design: Jobs",
  "Design: Tooling",
  "Design",
] as const;

const designGroups: DesignGroup[] = [
  {
    slug: "core",
    title: "Core Design",
    section: "Design: Core",
    description:
      "Cross-cutting Trellis architecture, storage, typing, service, and observability patterns.",
    href: "/design/core",
  },
  {
    slug: "auth",
    title: "Auth Design",
    section: "Design: Auth",
    description:
      "Identity, approvals, auth protocol, public APIs, and auth operations guidance.",
    href: "/design/auth",
  },
  {
    slug: "contracts",
    title: "Contracts Design",
    section: "Design: Contracts",
    description:
      "Canonical contract model, SDK derivation rules, and contract-runtime invariants.",
    href: "/design/contracts",
  },
  {
    slug: "operations",
    title: "Operations Design",
    section: "Design: Operations",
    description:
      "Caller-visible async workflow semantics, durability, authorization, and runtime invariants.",
    href: "/design/operations",
  },
  {
    slug: "jobs",
    title: "Jobs Design",
    section: "Design: Jobs",
    description:
      "Service-private background execution, worker lifecycle, and job state rules.",
    href: "/design/jobs",
  },
  {
    slug: "tooling",
    title: "Tooling Design",
    section: "Design: Tooling",
    description:
      "Trellis tooling surfaces, currently centered on the CLI design.",
    href: "/design/tooling",
  },
];

function designSectionOrderIndex(section: string) {
  return designGroupOrder.indexOf(section as (typeof designGroupOrder)[number]);
}

const acronymWords = new Map([
  ["api", "API"],
  ["auth", "Auth"],
  ["cli", "CLI"],
  ["kv", "KV"],
  ["nats", "NATS"],
  ["rust", "Rust"],
  ["svelte", "Svelte"],
  ["trellis", "Trellis"],
  ["typescript", "TypeScript"],
]);

function normalizeDesignPath(path: string) {
  return path
    .replace(/^\$design\//, "")
    .replace(/^.*\/design\//, "")
    .replace(/\\/g, "/")
    .replace(/\.md$/, "");
}

function designRelativePathFromPath(path: string) {
  return normalizeDesignPath(path);
}

function designSlugFromPath(path: string) {
  return designRelativePathFromPath(path).replace(/(?:^|\/)README$/i, "");
}

function designFlatSlugFromPath(path: string) {
  const relativePath = designRelativePathFromPath(path);
  const basename = relativePath.split("/").filter(Boolean).pop() ??
    relativePath;
  return basename.replace(/(?:^|\/)README$/i, "");
}

function designSectionFromPath(path: string) {
  const relativePath = designRelativePathFromPath(path);
  const [group] = relativePath.split("/");

  if (relativePath === "README") {
    return "Design Overview";
  }

  switch (group) {
    case "core":
      return "Design: Core";
    case "auth":
      return "Design: Auth";
    case "contracts":
      return "Design: Contracts";
    case "jobs":
      return "Design: Jobs";
    case "operations":
      return "Design: Operations";
    case "tooling":
      return "Design: Tooling";
    default:
      return "Design";
  }
}

function parseFrontmatter(raw: string) {
  const match = raw.match(/^---\n([\s\S]*?)\n---\n*/);
  if (!match) {
    return { body: raw, fields: new Map<string, string>() };
  }

  const fields = new Map<string, string>();
  for (const line of match[1].split("\n")) {
    const parts = line.match(/^([A-Za-z0-9_-]+):\s*(.+)$/);
    if (!parts) {
      continue;
    }
    fields.set(parts[1].toLowerCase(), parts[2].trim().replace(/^"|"$/g, ""));
  }

  return {
    body: raw.slice(match[0].length),
    fields,
  };
}

function markdownOrderFromRaw(raw: string) {
  const { fields } = parseFrontmatter(raw);
  const value = fields.get("order");
  if (!value) {
    return Number.POSITIVE_INFINITY;
  }

  const order = Number(value);
  return Number.isFinite(order) ? order : Number.POSITIVE_INFINITY;
}

function markdownTitleFromRaw(raw: string, fallbackSlug: string) {
  const { body, fields } = parseFrontmatter(raw);
  const explicitTitle = fields.get("title");
  if (explicitTitle) {
    return explicitTitle;
  }

  const headingMatch = body.match(/^#\s+(.+)$/m);
  if (!headingMatch) {
    return designTitleFromSlug(fallbackSlug);
  }

  return headingMatch[1]
    .replace(/^Design:\s*/i, "")
    .replace(/^Trellis\s+/i, (value) => value)
    .trim();
}

function markdownDescriptionFromRaw(raw: string, fallbackSlug: string) {
  const { body, fields } = parseFrontmatter(raw);
  const explicitDescription = fields.get("description");
  if (explicitDescription) {
    return explicitDescription;
  }

  const bodyWithoutFirstHeading = body.replace(/^#\s+.+$\n*/m, "").trim();
  const paragraphMatch = bodyWithoutFirstHeading.match(
    /^([^#\-*`|>\n].*(?:\n(?!\n|#|-|\*|`|\||>).+)*)/m,
  );
  if (!paragraphMatch) {
    return designDescriptionFromSlug(fallbackSlug);
  }

  return paragraphMatch[1].replace(/\s+/g, " ").trim();
}

function designTitleFromSlug(slug: string) {
  const titleSource = slug.split("/").filter(Boolean).pop() ?? slug;

  if (!titleSource) {
    return "Trellis Design Index";
  }

  return slug
    .split("/")
    .filter(Boolean)
    .pop()
    ?.split("-")
    .map((part) => {
      const lower = part.toLowerCase();
      if (acronymWords.has(lower)) {
        return acronymWords.get(lower);
      }
      return lower.charAt(0).toUpperCase() + lower.slice(1);
    })
    .join(" ") ?? titleSource;
}

function designDescriptionFromSlug(slug: string) {
  if (!slug) {
    return "How the Trellis design docs are organized and which ones to read for a given task.";
  }

  const title = designTitleFromSlug(slug);
  if (slug.startsWith("trellis-")) {
    return `${title} system design.`;
  }

  return `${title} design reference.`;
}

const designDocs: DocEntry[] = Object.keys(designModules)
  .map((path) => ({
    slug: designSlugFromPath(path),
    flatSlug: designFlatSlugFromPath(path),
    section: designSectionFromPath(path),
    raw: designRawModules[path] ?? "",
  }))
  .sort((a, b) => {
    const sectionDiff = designSectionOrderIndex(a.section) -
      designSectionOrderIndex(b.section);
    if (sectionDiff !== 0) return sectionDiff;
    const orderDiff = markdownOrderFromRaw(a.raw) - markdownOrderFromRaw(b.raw);
    if (orderDiff !== 0) return orderDiff;
    if (!a.slug) return -1;
    if (!b.slug) return 1;
    if (!a.flatSlug) return -1;
    if (!b.flatSlug) return 1;
    if (
      a.flatSlug.startsWith("trellis-") && !b.flatSlug.startsWith("trellis-")
    ) return -1;
    if (
      !a.flatSlug.startsWith("trellis-") && b.flatSlug.startsWith("trellis-")
    ) return 1;
    return designTitleFromSlug(a.flatSlug).localeCompare(
      designTitleFromSlug(b.flatSlug),
    );
  })
  .filter(({ slug }) => slug.length > 0)
  .map(({ slug, flatSlug, section, raw }) => ({
    title: markdownTitleFromRaw(raw, flatSlug),
    description: markdownDescriptionFromRaw(raw, flatSlug),
    href: `/design/${slug}`,
    section,
    showPageHeader: false,
  }));

const designOverviewDoc: DocEntry = {
  title: "Trellis Design Index",
  description:
    "How the Trellis design docs are organized and which ones to read for a given task.",
  href: "/design",
  section: "Design Overview",
  showPageHeader: false,
};

const designGroupDocs: DocEntry[] = designGroups.map((group) => ({
  title: group.title,
  description: group.description,
  href: group.href,
  section: group.section,
  showPageHeader: false,
}));

export const apiReferenceOverviewDoc: DocEntry = {
  title: "API Reference",
  description:
    "Generated TypeScript API docs and Rustdoc links for public Trellis crates.",
  href: "/api",
  section: "API Reference",
  showPageHeader: false,
};

export const apiReferenceDocs: DocEntry[] = [
  {
    title: "@qlever-llc/trellis",
    description:
      "TypeScript Trellis client runtime, contract helpers, generated SDKs, and service APIs.",
    href: "/api/typescript/trellis/index.ts/index.html",
    section: "API Reference",
  },
  {
    title: "@qlever-llc/trellis-svelte",
    description: "Svelte integration APIs for Trellis browser applications.",
    href: "/api/typescript/trellis-svelte/src/index.ts/index.html",
    section: "API Reference",
  },
  {
    title: "@qlever-llc/trellis-test",
    description:
      "Deno-first integration test helpers for Trellis service repositories.",
    href: "/api/typescript/trellis-test/index.ts/index.html",
    section: "API Reference",
  },
  {
    title: "@qlever-llc/result",
    description:
      "Class-based Result and AsyncResult APIs for explicit TypeScript error handling.",
    href: "/api/typescript/result/mod.ts/index.html",
    section: "API Reference",
  },
  {
    title: "trellis",
    description:
      "Curated Rust runtime facade and Trellis-owned SDK modules on docs.rs.",
    href: "https://docs.rs/trellis/latest/trellis/",
    section: "Rustdoc",
  },
  {
    title: "trellis-protocol",
    description:
      "Language-neutral Trellis artifacts, exact permissions, and signed authorization protocol primitives on docs.rs.",
    href: "https://docs.rs/trellis-protocol/latest/trellis_protocol/",
    section: "Rustdoc",
  },
];

export const pendingRustdocCrates = [] as const;

const typescriptServiceTutorialGroup = "Write a service";

const typescriptServiceTutorialDocs: DocEntry[] = [
  {
    title: "Tutorial: Write a service",
    description:
      "Build the same orders-service tutorial in TypeScript or Rust.",
    href: "/guides/write-a-service",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "Overview",
  },
  {
    title: "Set up the project",
    description:
      "Create the standalone orders-service project and install Trellis dependencies.",
    href: "/guides/write-a-service/setup",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "1. Setup",
  },
  {
    title: "Your first API and participant",
    description:
      "Declare the smallest valid Trellis API and service participant in native IDL.",
    href: "/guides/write-a-service/first-contract",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "2. IDL",
  },
  {
    title: "The service entry point",
    description:
      "Connect the service runtime, provision an instance, and run the service locally.",
    href: "/guides/write-a-service/service-entrypoint",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "3. Entry point",
  },
  {
    title: "Adding a database",
    description:
      "Declare a service-owned KV bucket and open a typed KV client from the runtime binding.",
    href: "/guides/write-a-service/kv-store",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "4. KV store",
  },
  {
    title: "Writing our first RPC",
    description:
      "Add Orders.Create schemas, its native API declaration, capability gate, and handler.",
    href: "/guides/write-a-service/first-rpc",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "5. First RPC",
  },
  {
    title: "Retrieving an order with service errors",
    description:
      "Add Orders.Get schemas, a declared domain error, and a typed read handler.",
    href: "/guides/write-a-service/retrieve-order",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "6. Retrieve order",
  },
  {
    title: "Listening to the outside world",
    description:
      "Declare an auth event dependency and subscribe to Auth.Connections.Opened.",
    href: "/guides/write-a-service/listen-events",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "7. Listen for Events",
  },
  {
    title: "Publishing your own events",
    description:
      "Declare and publish an Orders.Shipped event for downstream services.",
    href: "/guides/write-a-service/publish-events",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "8. Publish Events",
  },
  {
    title: "Use a feed for filtered live views",
    description:
      "Expose caller-filtered live views instead of forwarding broad service events to browsers.",
    href: "/guides/write-a-service/feeds",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "9. Feeds",
  },
  {
    title: "Handle graceful shutdown",
    description:
      "Stop the service cleanly on SIGTERM so local restarts and deployments drain work safely.",
    href: "/guides/write-a-service/shutdown",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "10. Shutdown",
  },
  {
    title: "Generate SDKs and artifacts",
    description:
      "Compile native IDL into canonical artifacts and private language SDKs.",
    href: "/guides/write-a-service/contract-artifacts",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "11. Generation",
  },
  {
    title: "Declare optional dependencies",
    description:
      "Use optional uses for additive integrations that should not block service activation.",
    href: "/guides/write-a-service/optional-dependencies",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "12. Optional uses",
  },
  {
    title: "Development loop",
    description:
      "Know when to regenerate artifacts, review deployment authority, and restart the service.",
    href: "/guides/write-a-service/development-loop",
    section: "Getting started",
    sidebarGroup: typescriptServiceTutorialGroup,
    sidebarLabel: "13. Dev loop",
  },
];

const conceptDocs: DocEntry[] = [
  {
    title: "What is Trellis?",
    description:
      "A minimally technical overview of why Trellis exists, how it compares with REST and pub/sub, and what it gives teams building connected data systems.",
    href: "/guides/overview",
    section: "Introduction",
  },
  {
    title: "Trellis Concepts",
    description:
      "Deeper concepts behind Trellis, including platform boundaries, contract-driven authority, transport surfaces, resources, and generated APIs.",
    href: "/guides/concepts",
    section: "Introduction",
  },
  {
    title: "Architecture",
    description:
      "Service categories, platform boundaries, runtime ownership, and public cross-service surfaces.",
    href: "/guides/concepts/architecture",
    section: "Trellis Concepts",
  },
  {
    title: "Contracts",
    description:
      "Service and app contracts, contract identity, and generated artifacts.",
    href: "/guides/concepts/contracts",
    section: "Trellis Concepts",
  },
  {
    title: "Deployment authority",
    description:
      "Deployment authority, identity authority, authority updates, reconciliation, availability, liveness, grant overrides, and contract evidence.",
    href: "/guides/concepts/deployment-authority",
    section: "Trellis Concepts",
  },
  {
    title: "Communication",
    description:
      "RPCs, operations, events, feeds, cross-contract dependencies, and surface availability.",
    href: "/guides/concepts/communication",
    section: "Trellis Concepts",
  },
  {
    title: "Authentication and authorization",
    description:
      "Principals, session keys, browser login, portals, approvals, and capabilities.",
    href: "/guides/concepts/authentication-and-authorization",
    section: "Trellis Concepts",
  },
  {
    title: "Resources",
    description:
      "KV buckets, store resources, public app state, and runtime stream semantics.",
    href: "/guides/concepts/resources",
    section: "Trellis Concepts",
  },
  {
    title: "Files and transfers",
    description:
      "Public file APIs, service-owned stores, and operation-native byte transfer.",
    href: "/guides/concepts/files-and-transfers",
    section: "Trellis Concepts",
  },
  {
    title: "Type system and errors",
    description:
      "Schemas, validation, declared errors, Result values, pagination, and stable storage identity.",
    href: "/guides/concepts/type-system-and-errors",
    section: "Trellis Concepts",
  },
  {
    title: "Devices and activation",
    description:
      "Preregistered devices, activation portals, device deployments, review policy, and online credentials.",
    href: "/guides/concepts/devices-and-activation",
    section: "Trellis Concepts",
  },
  {
    title: "Jobs",
    description:
      "Service-private background execution and when to use operations instead.",
    href: "/guides/concepts/jobs",
    section: "Trellis Concepts",
  },
];

export const guideDocs: DocEntry[] = [
  ...conceptDocs,
  {
    title: "Libraries",
    description:
      "How Trellis programs use runtime libraries, generated SDKs, and API reference docs.",
    href: "/guides/libraries",
    section: "Libraries",
  },
  {
    title: "TypeScript libraries",
    description:
      "Use Trellis from TypeScript apps, services, devices, portals, and CLIs with generated SDKs and surface-first APIs.",
    href: "/guides/libraries/typescript",
    section: "Libraries",
  },
  {
    title: "Rust libraries",
    description:
      "Use Trellis from Rust services, CLIs, devices, and generated Cargo participant facades.",
    href: "/guides/libraries/rust",
    section: "Libraries",
  },
  {
    title: "Testing Trellis services",
    description:
      "Run out-of-tree TypeScript service integration tests with @qlever-llc/trellis-test, case-scoped fixtures, and the generic runner.",
    href: "/guides/testing-trellis-services",
    section: "Libraries",
  },
  {
    title: "Install the Trellis CLI",
    description:
      "Install the trellis command-line tool from a release, Cargo, or the current checkout.",
    href: "/guides/install-trellis-cli",
    section: "Installing Trellis",
  },
  {
    title: "Installing Trellis",
    description:
      "Generate a local NATS bundle, then run Trellis and Console from source.",
    href: "/guides/starting-trellis",
    section: "Installing Trellis",
  },
  ...typescriptServiceTutorialDocs,
  {
    title: "Jobs: TypeScript",
    description:
      "Add background job processing to a TypeScript service with retry, progress tracking, and dead-letter handling.",
    href: "/guides/using-jobs-ts",
    section: "Features",
    sidebarGroup: "Jobs",
    sidebarLabel: "TS",
  },
  {
    title: "Jobs: Rust",
    description:
      "Add background job processing to a Rust service with retry, progress tracking, and dead-letter handling.",
    href: "/guides/using-jobs-rust",
    section: "Features",
    sidebarGroup: "Jobs",
    sidebarLabel: "Rust",
  },
  {
    title: "Operations: TypeScript",
    description:
      "Expose caller-visible async workflows from a TypeScript service with typed progress and cancellation.",
    href: "/guides/using-operations-ts",
    section: "Features",
    sidebarGroup: "Operations",
    sidebarLabel: "TS",
  },
  {
    title: "Operations: Rust",
    description:
      "Expose caller-visible async workflows from a Rust service with typed progress and cancellation.",
    href: "/guides/using-operations-rust",
    section: "Features",
    sidebarGroup: "Operations",
    sidebarLabel: "Rust",
  },
  {
    title: "Store resources: TypeScript",
    description:
      "Declare and use service-owned `resources.store` from a TypeScript service for large unstructured blobs.",
    href: "/guides/using-store-resources-ts",
    section: "Features",
    sidebarGroup: "Resources",
    sidebarLabel: "Store",
  },
  {
    title: "State: TypeScript",
    description:
      "Use Trellis-managed `State.*` RPCs from TypeScript apps and devices for semi-durable cloud-backed app memory with contract-lineage storage and state-version migrations.",
    href: "/guides/using-state-ts",
    section: "Features",
    sidebarGroup: "Resources",
    sidebarLabel: "State",
  },
  {
    title: "Write a SvelteKit app",
    description:
      "A working browser app that authenticates with Trellis and calls RPCs.",
    href: "/guides/writing-sveltekit-apps",
    section: "Getting started",
  },
  {
    title: "Create a custom portal",
    description:
      "Build and register a custom SvelteKit portal app for provider selection and contract approval.",
    href: "/guides/creating-custom-portal",
    section: "Advanced",
  },
  {
    title: "Devices",
    description:
      "Build a Trellis device that displays a QR activation URL and completes the device activation flow.",
    href: "/guides/devices",
    section: "Advanced",
  },
  {
    title: "Develop Trellis services with AI agents",
    description:
      "Use llms.txt, llms-full.txt, language-specific LLM guides, and a service AGENTS.md template to guide AI agents working in Trellis service repos.",
    href: "/guides/ai/developing-trellis-services",
    section: "Advanced",
  },
  {
    title: "Install a service from an image",
    description:
      "Deploy a published service from an OCI image into a running Trellis environment.",
    href: "/guides/install-service-from-image",
    section: "Administration",
  },
  {
    title: "Install a service from source",
    description:
      "Install and run a service from its source tree during development.",
    href: "/guides/install-service-from-source",
    section: "Administration",
  },
  {
    title: "Administer Jobs",
    description:
      "Query, cancel, replay, and monitor jobs across all services using the built-in Jobs API.",
    href: "/guides/administering-jobs",
    section: "Administration",
  },
  {
    title: "In-repo development",
    description:
      "Run the Trellis server, console, and NATS from the source tree for local development.",
    href: "/guides/in-repo-development",
    section: "Contributing",
  },
  {
    title: "Releasing Trellis",
    description:
      "Cut a Trellis release with the Rust xtask release tooling, changelog checks, verification, tagging, and publish flow.",
    href: "/guides/releasing-trellis",
    section: "Contributing",
  },
];

export const designNavigationDocs: DocEntry[] = [
  designOverviewDoc,
  ...designGroupDocs,
  ...designDocs,
];

export function getGuideDoc(pathname: string): DocEntry | null {
  return guideDocs.find((doc) => doc.href === pathname) ?? null;
}

export function getGuidePrevNext(pathname: string): {
  prev: DocEntry | null;
  next: DocEntry | null;
} {
  const index = guideDocs.findIndex((doc) => doc.href === pathname);
  if (index === -1) {
    return { prev: null, next: null };
  }
  return {
    prev: guideDocs[index - 1] ?? null,
    next: guideDocs[index + 1] ?? null,
  };
}

export function guideDocsBySection(): { section: string; docs: DocEntry[] }[] {
  const orderedSections = Array.from(
    new Set(guideDocs.map((doc) => doc.section)),
  );

  return orderedSections.map((section) => ({
    section,
    docs: guideDocs.filter((doc) => doc.section === section),
  }));
}

function guideSidebarItemsForSection(docs: DocEntry[]): SidebarItem[] {
  const items: SidebarItem[] = [];
  const groupIndexes = new Map<string, number>();

  for (const doc of docs) {
    if (!doc.sidebarGroup) {
      items.push({ kind: "doc", doc });
      continue;
    }

    const existingIndex = groupIndexes.get(doc.sidebarGroup);
    if (existingIndex === undefined) {
      const docs = doc.sidebarLabel === "Overview" ? [] : [doc];
      groupIndexes.set(doc.sidebarGroup, items.length);
      items.push({
        kind: "group",
        label: doc.sidebarGroup,
        parent: doc,
        docs,
      });
      continue;
    }

    const entry = items[existingIndex];
    if (entry.kind === "group") {
      entry.docs.push(doc);
    }
  }

  return items;
}

export function guideSidebarBySection(): GuideSidebarSection[] {
  const sidebarDocs = guideDocs.filter((doc) =>
    !doc.href.startsWith("/guides/concepts")
  );
  const orderedSections = Array.from(
    new Set(sidebarDocs.map((doc) => doc.section)),
  );

  return orderedSections.map((section) => ({
    section,
    items: guideSidebarItemsForSection(
      sidebarDocs.filter((doc) => doc.section === section),
    ),
  }));
}

export function getDesignDoc(pathname: string): DocEntry | null {
  return designNavigationDocs.find((doc) => doc.href === pathname) ?? null;
}

export function getDesignPrevNext(pathname: string): {
  prev: DocEntry | null;
  next: DocEntry | null;
} {
  const index = designNavigationDocs.findIndex((doc) => doc.href === pathname);
  if (index === -1) {
    return { prev: null, next: null };
  }

  return {
    prev: designNavigationDocs[index - 1] ?? null,
    next: designNavigationDocs[index + 1] ?? null,
  };
}

export function designDocsBySection(): { section: string; docs: DocEntry[] }[] {
  return designGroupOrder
    .filter((section) =>
      designNavigationDocs.some((doc) => doc.section === section)
    )
    .map((section) => ({
      section,
      docs: designNavigationDocs.filter((doc) => doc.section === section),
    }));
}

export function overviewDocsBySection(): {
  section: string;
  docs: DocEntry[];
}[] {
  return [
    ...guideDocsBySection(),
    {
      section: "API Reference",
      docs: [apiReferenceOverviewDoc],
    },
    {
      section: "Design",
      docs: designGroupDocs,
    },
  ].filter((group) => group.docs.length > 0);
}

export function allPrimaryNavDocs(): DocEntry[] {
  return [
    ...guideDocs,
    apiReferenceOverviewDoc,
    designOverviewDoc,
    ...designGroupDocs,
  ];
}

export function allDocsByHref(pathname: string): DocEntry | null {
  return [
    ...guideDocs,
    apiReferenceOverviewDoc,
    ...apiReferenceDocs,
    ...designNavigationDocs,
  ].find((doc) => doc.href === pathname) ?? null;
}

export function docsForSection(section: string): DocEntry[] {
  return [
    ...guideDocs,
    apiReferenceOverviewDoc,
    ...apiReferenceDocs,
    ...designNavigationDocs,
  ].filter((doc) => doc.section === section);
}

export function docsBySection(): { section: string; docs: DocEntry[] }[] {
  const orderedSections = [
    ...guideDocsBySection(),
    {
      section: "API Reference",
      docs: [apiReferenceOverviewDoc, ...apiReferenceDocs],
    },
    ...designDocsBySection(),
  ];

  return orderedSections.map(({ section, docs }) => ({
    section,
    docs,
  }));
}

export function getDesignGroup(slug: string): DesignGroup | null {
  return designGroups.find((group) => group.slug === slug) ?? null;
}

export function getDocsForDesignGroup(slug: string): DocEntry[] {
  const group = getDesignGroup(slug);
  if (!group) {
    return [];
  }

  return designDocs.filter((doc) => doc.section === group.section);
}
