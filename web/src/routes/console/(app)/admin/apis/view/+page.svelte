<script lang="ts">
  import { apis } from "trellis-web-generated";
  import { page } from "$app/state";
  import { onDestroy, untrack } from "svelte";
  import { getTrellis } from "$lib/trellis";
  import { consoleUrl } from "$lib/console_paths";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { formatDate } from "$lib/format";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import PageToolbar from "$lib/components/PageToolbar.svelte";
  import SchemaView from "$lib/components/SchemaView.svelte";

  type Detail = apis.auth.ApisGetOutput;
  type Action = Detail["actions"][number];
  type Kind = Action["kind"];

  const KINDS: { kind: Kind; title: string; plural: string }[] = [
    { kind: "rpc", title: "Request", plural: "Requests" },
    { kind: "operation", title: "Operation", plural: "Operations" },
    { kind: "event", title: "Event", plural: "Events" },
    { kind: "live", title: "Live feed", plural: "Live feeds" },
  ];
  const DIRECTIONS: Record<string, string> = {
    call: "Call",
    invoke: "Start",
    observe: "Follow",
    cancel: "Cancel",
    control: "Signal",
    publish: "Publish",
    subscribe: "Receive",
  };

  const trellis = getTrellis();
  const scope = new RequestScope("api-view");
  const apiId = $derived(page.url.searchParams.get("apiId") ?? "");
  let detail = $state.raw<Detail | null>(null);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let tab = $state<"actions" | "permissions" | "idl">("actions");
  let kind = $state<Kind | "all">("all");
  let search = $state("");
  let selectedKey = $state<string | null>(null);

  const keyOf = (action: Action) => `${action.kind}:${action.name}`;
  const kindTitle = (value: string) =>
    KINDS.find((item) => item.kind === value)?.title ?? value;

  const shown = $derived(
    (detail?.actions ?? []).filter(
      (action) =>
        (kind === "all" || action.kind === kind) &&
        `${action.name} ${action.description ?? ""}`
          .toLowerCase()
          .includes(search.trim().toLowerCase()),
    ),
  );
  const selected = $derived(
    shown.find((action) => keyOf(action) === selectedKey) ?? shown[0] ?? null,
  );
  const capabilities = $derived(
    (detail?.api.capabilities ?? []).filter(
      (item) => item.kind === "capability",
    ),
  );
  const groups = $derived(
    (detail?.api.capabilities ?? []).filter((item) => item.kind === "group"),
  );

  /** The permissions that let someone use an action, by what they let them do. */
  function allowedBy(
    action: Action,
  ): { direction: string; titles: string[] }[] {
    const byDirection = new Map<string, string[]>();
    for (const capability of capabilities)
      for (const allow of capability.allows)
        if (allow.name === action.name && allow.kind === action.kind)
          byDirection.set(allow.direction, [
            ...(byDirection.get(allow.direction) ?? []),
            capability.title,
          ]);
    return [...byDirection].map(([direction, titles]) => ({
      direction: DIRECTIONS[direction] ?? direction,
      titles,
    }));
  }

  function select(name: string, actionKind: Kind): void {
    tab = "actions";
    kind = "all";
    search = "";
    selectedKey = `${actionKind}:${name}`;
  }

  async function load(id: string): Promise<void> {
    const token = scope.begin();
    loading = true;
    error = null;
    detail = null;
    try {
      const response = await trellis.apisGet({ apiId: id }).orThrow();
      if (scope.isCurrent(token)) detail = response;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  $effect(() => {
    const id = apiId;
    untrack(() => {
      selectedKey = page.url.searchParams.get("action");
      void load(id);
    });
  });
  onDestroy(() => scope.dispose());
</script>

{#snippet schemaSection(title: string, schema: unknown)}
  {#if schema !== undefined}
    <section class="space-y-1">
      <h3 class="text-sm font-semibold">{title}</h3>
      <SchemaView {schema} />
    </section>
  {/if}
{/snippet}

<section class="space-y-5">
  <a
    class="inline-flex items-center gap-1 text-sm text-base-content/65 hover:text-base-content"
    href={consoleUrl("/admin/apis")}
    ><Icon name="chevronDown" size={14} class="rotate-90" />All APIs</a
  >
  {#if loading}
    <LoadingState label="Loading API" />
  {:else if error}
    <Notice variant="error">{error}</Notice>
  {:else if detail}
    {@const api = detail.api}
    <PageToolbar title={api.title} description={api.description}>
      {#snippet meta()}
        {#if api.version}<span class="badge badge-neutral">{api.version}</span
          >{/if}
      {/snippet}
    </PageToolbar>

    <dl class="api-facts">
      <div>
        <dt>API</dt>
        <dd class="flex items-center gap-1">
          <code>{api.apiId}</code><CopyButton
            value={api.apiId}
            label="Copy API ID"
          />
        </dd>
      </div>
      {#each KINDS as item (item.kind)}
        {@const total = detail.actions.filter(
          (action) => action.kind === item.kind,
        ).length}
        {#if total}<div>
            <dt>{item.plural}</dt>
            <dd>{total}</dd>
          </div>{/if}
      {/each}
      <div>
        <dt>Permissions</dt>
        <dd>{capabilities.length}</dd>
      </div>
      <div>
        <dt>Installed</dt>
        <dd>{formatDate(api.acceptedAt)}</dd>
      </div>
    </dl>

    <div class="trellis-segment" role="tablist" aria-label="Show">
      {#each [["actions", "Actions"], ["permissions", "Permissions"], ["idl", "IDL"]] as const as [value, label] (value)}
        <button
          type="button"
          role="tab"
          aria-selected={tab === value}
          class={tab === value ? "active" : ""}
          onclick={() => (tab = value)}>{label}</button
        >
      {/each}
    </div>

    {#if tab === "actions"}
      <div class="api-explorer">
        <div class="trellis-section min-w-0 p-3">
          <input
            class="input input-sm w-full"
            type="search"
            placeholder="Find an action"
            aria-label="Find an action"
            bind:value={search}
          />
          <div class="mt-2 flex flex-wrap gap-1">
            {#each [{ kind: "all", plural: "All" }, ...KINDS] as item (item.kind)}
              {#if item.kind === "all" || detail.actions.some((action) => action.kind === item.kind)}
                <button
                  type="button"
                  class={["trellis-chip", kind === item.kind && "active"]}
                  aria-pressed={kind === item.kind}
                  onclick={() => (kind = item.kind as Kind | "all")}
                  >{item.plural}</button
                >
              {/if}
            {/each}
          </div>
          <ul class="mt-2" aria-label="Actions">
            {#each shown as action (keyOf(action))}
              <li>
                <button
                  type="button"
                  class={[
                    "action-row",
                    selected && keyOf(selected) === keyOf(action) && "active",
                  ]}
                  aria-current={selected && keyOf(selected) === keyOf(action)
                    ? "true"
                    : undefined}
                  onclick={() => (selectedKey = keyOf(action))}
                >
                  <span class="flex items-center gap-2">
                    <code class="truncate font-medium">{action.name}</code>
                    <span class="badge badge-ghost badge-xs shrink-0"
                      >{kindTitle(action.kind)}</span
                    >
                  </span>
                  {#if action.description}<span
                      class="block truncate text-xs text-base-content/60"
                      >{action.description}</span
                    >{/if}
                </button>
              </li>
            {:else}
              <li class="px-2 py-6 text-center text-sm text-base-content/60">
                No actions match.
              </li>
            {/each}
          </ul>
        </div>

        <div class="trellis-section min-w-0 space-y-5 p-5">
          {#if selected}
            <header class="space-y-1">
              <p class="text-xs text-base-content/60">
                {kindTitle(selected.kind)}
              </p>
              <h2
                class="flex flex-wrap items-center gap-2 text-lg font-semibold"
              >
                <code>{selected.name}</code>
                {#if selected.paginated}<span class="badge badge-sm">Paged</span
                  >{/if}
                {#if selected.upload}<span class="badge badge-sm"
                    >Accepts an upload</span
                  >{/if}
                {#if selected.stream}<span class="badge badge-sm"
                    >Streams the upload</span
                  >{/if}
                {#if selected.download}<span class="badge badge-sm"
                    >Can return a download</span
                  >{/if}
              </h2>
              {#if selected.description}<p
                  class="whitespace-pre-line text-base-content/75"
                >
                  {selected.description}
                </p>{:else}<p class="text-sm text-base-content/55">
                  No description.
                </p>{/if}
            </header>

            <section class="space-y-1">
              <h3 class="text-sm font-semibold">Who can use it</h3>
              {#each allowedBy(selected) as entry (entry.direction)}
                <p class="text-sm">
                  <span class="text-base-content/60">{entry.direction}:</span>
                  {entry.titles.join(", ")}
                </p>
              {:else}
                <p class="text-sm text-base-content/60">
                  No permission lists it.
                </p>
              {/each}
            </section>

            {@render schemaSection(
              selected.kind === "live" ? "Request" : "Input",
              selected.input,
            )}
            {@render schemaSection(
              selected.kind === "operation" ? "Result" : "Output",
              selected.output,
            )}
            {@render schemaSection("Progress", selected.progress)}
            {@render schemaSection("Live updates", selected.update)}
            {@render schemaSection(
              selected.kind === "live" ? "Each item" : "Data",
              selected.payload,
            )}
            {#if selected.params.length}
              <section class="space-y-1">
                <h3 class="text-sm font-semibold">Routed by</h3>
                <p class="text-sm">
                  {#each selected.params as param, index (param)}{index
                      ? ", "
                      : ""}<code>{param}</code>{/each}
                </p>
              </section>
            {/if}
            {#each Object.entries(selected.signals) as [name, schema] (name)}
              {@render schemaSection(`Signal ${name}`, schema)}
            {/each}
            {#if selected.errors.length}
              <section class="space-y-2">
                <h3 class="text-sm font-semibold">Errors</h3>
                {#each selected.errors as failure (failure.name)}
                  <div>
                    <code class="text-sm font-medium">{failure.name}</code>
                    {#if failure.payload !== undefined}<SchemaView
                        schema={failure.payload}
                      />{/if}
                  </div>
                {/each}
              </section>
            {/if}
          {:else}
            <EmptyState title="Choose an action" />
          {/if}
        </div>
      </div>
    {:else if tab === "permissions"}
      <div class="trellis-section divide-y divide-base-300">
        {#each [...groups, ...capabilities] as capability (capability.identity.capabilityId)}
          <article class="space-y-1 p-5">
            <h2 class="flex flex-wrap items-center gap-2 font-semibold">
              {capability.title}
              {#if capability.kind === "group"}<span class="badge badge-sm"
                  >Group of {capability.members.length}</span
                >{/if}
            </h2>
            <p class="text-sm text-base-content/70">{capability.description}</p>
            <p class="text-xs text-base-content/55">
              <code>{capability.identity.capabilityId}</code>
            </p>
            {#if capability.kind === "group"}
              <p class="pt-1 text-sm">
                <span class="text-base-content/60">Includes</span>
                {capabilities
                  .filter((member) =>
                    capability.members.includes(member.identity.capabilityId),
                  )
                  .map((member) => member.title)
                  .join(", ")}
              </p>
            {:else if capability.allows.length}
              <p class="pt-1 text-sm">
                <span class="text-base-content/60">Allows</span>
                {#each capability.allows as allow, index (`${allow.kind}:${allow.name}:${allow.direction}`)}{index
                    ? ", "
                    : " "}<button
                    type="button"
                    class="link link-hover"
                    onclick={() => select(allow.name, allow.kind)}
                    ><code>{allow.name}</code></button
                  >{/each}
              </p>
            {/if}
          </article>
        {:else}
          <EmptyState class="m-4" title="This API offers no permissions" />
        {/each}
      </div>
    {:else}
      <div class="trellis-section p-4">
        <div class="mb-2 flex justify-end">
          <CopyButton value={detail.canonicalIdl} label="Copy IDL" />
        </div>
        <pre
          class="max-h-[40rem] overflow-auto rounded-field bg-base-200/50 p-4 text-xs leading-relaxed">{detail.canonicalIdl}</pre>
      </div>
    {/if}
  {/if}
</section>

<style>
  .api-facts {
    display: grid;
    gap: 0.75rem 1.5rem;
    grid-template-columns: repeat(auto-fit, minmax(9rem, max-content));
  }
  .api-facts dt {
    color: color-mix(in oklab, var(--color-base-content) 60%, transparent);
    font-size: 0.8125rem;
  }
  .api-facts dd {
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .api-explorer {
    align-items: start;
    display: grid;
    gap: 1rem;
    grid-template-columns: minmax(0, 1fr);
  }
  @media (min-width: 64rem) {
    .api-explorer {
      grid-template-columns: minmax(16rem, 22rem) minmax(0, 1fr);
    }
  }
  .action-row {
    background: transparent;
    border: 0;
    border-radius: 0.5rem;
    cursor: pointer;
    display: block;
    padding: 0.45rem 0.5rem;
    text-align: left;
    width: 100%;
  }
  .action-row:hover {
    background: color-mix(in oklab, var(--color-base-200) 70%, transparent);
  }
  .action-row.active {
    background: color-mix(in oklab, var(--color-primary) 10%, transparent);
  }
</style>
