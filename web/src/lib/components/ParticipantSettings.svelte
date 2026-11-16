<script lang="ts">
  import { apis } from "trellis-web-generated";
  import type { JsonValue } from "@oatscenter/trellis/generated";
  import { onMount } from "svelte";
  import { ulid } from "ulid";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { asSchema, definitionsOf, resolve } from "$lib/console/schema.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import Icon from "$lib/components/Icon.svelte";

  type View = apis.auth.ConfigGetOutput;
  type Field = View["fields"][number];
  type Revision = apis.auth.ConfigRevisionsListOutput["items"][number];

  // Settings a service or device declares, for the deployment or one of its
  // instances' overrides.
  // A service or device deployment, or an app's registered client.
  type Props = {
    deploymentId?: string;
    clientId?: string;
    /** The instance whose overrides are shown; empty for every instance. */
    instanceId?: string;
    /** Names to show for instances, by instance ID, instead of their titles. */
    instanceNames?: ReadonlyMap<string, string>;
    /** Set once loaded: whether there are settings to show. */
    available?: boolean;
  };
  let {
    deploymentId,
    clientId,
    instanceId = $bindable(""),
    instanceNames,
    available = $bindable(false),
  }: Props = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("participant-settings");
  const nameOf = (instance: { instanceId: string; title: string }) =>
    instanceNames?.get(instance.instanceId) ?? instance.title;
  let view = $state.raw<View | null>(null);
  // Whose settings `view` holds, so edits wait until it matches the selector.
  let loadedKey = $state("");
  // Every instance of the deployment, for choosing whose overrides to edit.
  let instances = $state.raw<View["instances"]>([]);
  let revisions = $state.raw<Revision[]>([]);
  // What the administrator typed for the loaded target, by field name.
  // Untouched fields are absent; switching targets discards them.
  let drafts = $state<Record<string, string>>({});
  // Secrets typed here that the administrator chose to see; a saved secret is
  // never sent back, so only a draft can be shown.
  let revealed = $state<Record<string, boolean>>({});
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const target = $derived(
    clientId
      ? { clientId }
      : { deploymentId, ...(instanceId ? { instanceId } : {}) },
  );
  const key = $derived(`${clientId ?? deploymentId}:${instanceId}`);
  // The shown settings belong to what the selector names.
  const ready = $derived(view !== null && loadedKey === key);
  const loadedInstanceId = $derived(view?.target.instanceId);
  const editable = $derived(canPerform(authority.authority, "configSet"));
  const changed = $derived(Object.keys(drafts).length > 0);
  // Instances still running restart-only values from an earlier save.
  const behind = $derived.by(() => {
    const current = view;
    if (!current?.fields.some((field) => field.restart)) return [];
    return current.instances.filter((instance) =>
      loadedInstanceId
        ? instance.connectedRevision !== current.deploymentRevision ||
          instance.connectedInstanceRevision !== current.revision
        : instance.connectedRevision !== current.revision,
    );
  });

  $effect(() => {
    scope.setKey(key);
    drafts = {};
    void load();
  });

  async function load(): Promise<void> {
    const token = scope.begin();
    const loading = key;
    try {
      const input = apis.auth.API.actions["rpc:Config.Get"].input.decode({
        target,
      });
      const loaded = await trellis.configGet(input).orThrow();
      const history = await trellis
        .configRevisionsList({ target: input.target, page: { limit: 20 } })
        .orThrow();
      if (!scope.isCurrent(token)) return;
      view = loaded;
      loadedKey = loading;
      if (!input.target.instanceId) {
        instances = loaded.instances;
        available = loaded.fields.length > 0;
      }
      revisions = history.items;
      error = null;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      scope.settle(token);
    }
  }

  // The JSON types a field's value may take.
  function types(field: Field): unknown[] {
    const root = asSchema(field.schema) ?? {};
    const defs = definitionsOf(root);
    const schema = resolve(root, defs);
    const options = Array.isArray(schema.anyOf)
      ? schema.anyOf.flatMap((option) => {
          const node = asSchema(option);
          return node ? [resolve(node, defs)] : [];
        })
      : [schema];
    return options.flatMap((option) =>
      Array.isArray(option.type) ? option.type : [option.type],
    );
  }

  // Text settings, nullable ones included, are typed as text; anything else
  // as JSON.
  function isText(field: Field): boolean {
    const kinds = types(field).filter((kind) => kind !== "null");
    return kinds.length === 1 && kinds[0] === "string";
  }

  function shown(field: Field): string {
    if (field.value === undefined) return "";
    if (field.value === null && isText(field)) return "";
    return typeof field.value === "string" && isText(field)
      ? field.value
      : JSON.stringify(field.value);
  }

  function placeholder(field: Field): string {
    if (field.secret) return field.isSet ? "Set — type to replace" : "Not set";
    if (field.value === null) return "No value";
    if (field.default === undefined) return "";
    return `Default: ${typeof field.default === "string" ? field.default : JSON.stringify(field.default)}`;
  }

  function sourceLabel(field: Field): string {
    switch (field.source) {
      case "instance":
        return "Set for this instance";
      case "deployment":
        return loadedInstanceId ? "Same as every instance" : "Set";
      case "default":
        return "Default";
      default:
        return "Not set";
    }
  }

  function parse(field: Field, text: string): JsonValue {
    if (isText(field)) return text;
    try {
      return JSON.parse(text) as JsonValue;
    } catch {
      throw new Error(`${field.title} must be JSON, such as 9090 or true.`);
    }
  }

  // Runs a change captured for `intentKey`. Its outcome is shown only while
  // the selector still names that target. On success the drafts of `settled`
  // fields go, or every draft when it is omitted; the rest stay pending.
  async function mutate(
    intentKey: string,
    run: () => Promise<View>,
    done: string,
    settled?: readonly string[],
  ): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    notice = null;
    try {
      const result = await run();
      if (key !== intentKey) return;
      view = result;
      loadedKey = intentKey;
      // A saved secret is never shown back, so what was typed goes at once.
      drafts = settled
        ? Object.fromEntries(
            Object.entries(drafts).filter(([name]) => !settled.includes(name)),
          )
        : {};
      notice = done;
      await load();
    } catch (cause) {
      if (key !== intentKey) return;
      error =
        classifyMutationError(cause).kind === "unknown"
          ? "We couldn't confirm the change. Refresh before trying again."
          : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  async function save(): Promise<void> {
    if (!ready || !view) return;
    const current = view;
    const values: Record<string, JsonValue> = {};
    const secrets: Record<string, JsonValue> = {};
    try {
      for (const [name, text] of Object.entries(drafts)) {
        const field = current.fields.find((item) => item.name === name);
        if (!field) continue;
        if (field.secret) {
          if (text !== "") secrets[name] = parse(field, text);
        } else values[name] = parse(field, text);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
      return;
    }
    const revision = current.revision;
    await mutate(
      loadedKey,
      () =>
        trellis
          .configSet({
            target: current.target,
            values,
            secrets,
            ...(revision === undefined ? {} : { expectedRevision: revision }),
            idempotencyKey: ulid(),
          })
          .orThrow(),
      "Settings saved.",
    );
  }

  // Unset removes the value stored here, so the default or deployment
  // applies; null stores null here, overriding them. Only this field's draft
  // is dropped; other unsaved edits stay pending.
  async function store(
    field: Field,
    change: "unset" | "null",
    done: string,
  ): Promise<void> {
    if (!ready || !view) return;
    const current = view;
    const revision = current.revision;
    const nulled = { [field.name]: null };
    await mutate(
      loadedKey,
      () =>
        trellis
          .configSet({
            target: current.target,
            ...(change === "unset"
              ? { unset: [field.name] }
              : field.secret
                ? { secrets: nulled }
                : { values: nulled }),
            ...(revision === undefined ? {} : { expectedRevision: revision }),
            idempotencyKey: ulid(),
          })
          .orThrow(),
      done,
      [field.name],
    );
  }

  async function restore(revision: Revision): Promise<void> {
    if (!ready || !view?.revision) return;
    const current = view;
    const intentKey = loadedKey;
    const expectedRevision = view.revision;
    const confirmed = await confirmationModal?.confirm({
      title: "Restore these settings?",
      message: `The settings saved ${formatDate(revision.createdAt)} become current again.`,
      confirmLabel: "Restore",
    });
    if (!confirmed || key !== intentKey) return;
    await mutate(
      intentKey,
      () =>
        trellis
          .configRollback({
            target: current.target,
            revision: revision.revision,
            expectedRevision,
            idempotencyKey: ulid(),
          })
          .orThrow(),
      "Earlier settings restored.",
    );
  }

  async function remove(revision: Revision): Promise<void> {
    if (!ready || !view) return;
    const current = view;
    const intentKey = loadedKey;
    const confirmed = await confirmationModal?.confirm({
      title: "Delete this earlier version?",
      message:
        "It can't be restored afterwards. Delete versions that hold replaced passwords or keys.",
      confirmLabel: "Delete version",
    });
    if (!confirmed || key !== intentKey) return;
    await mutate(
      intentKey,
      () =>
        trellis
          .configRevisionsDelete({
            target: current.target,
            revision: revision.revision,
            idempotencyKey: ulid(),
          })
          .orThrow(),
      "Earlier version deleted.",
    );
  }

  onMount(() => () => scope.dispose());
</script>

{#if view && (view.fields.length > 0 || instanceId)}
  <section
    id="settings"
    class="trellis-section space-y-4 p-5"
    aria-label="Settings"
  >
    <div class="flex flex-wrap items-center justify-between gap-3">
      <h2 class="text-base font-semibold">Settings</h2>
      {#if instances.length > 0 || instanceId}
        <label class="flex items-center gap-2 text-sm"
          ><span class="text-base-content/65">For</span><select
            class="select select-sm"
            bind:value={instanceId}
            ><option value="">Every instance</option
            >{#each instances as instance (instance.instanceId)}<option
                value={instance.instanceId}>{nameOf(instance)}</option
              >{/each}</select
          >{#if !ready && !error}<span
              class="loading loading-spinner loading-xs"
              aria-label="Loading settings"
            ></span>{/if}</label
        >
      {/if}
    </div>
    {#if notice}<Notice variant="success">{notice}</Notice>{/if}
    {#if error}<Notice variant="error">{error}</Notice>{/if}
    {#if behind.length > 0}<Notice variant="info"
        >{behind.map(nameOf).join(", ")}
        {behind.length === 1 ? "uses" : "use"} the settings from when
        {behind.length === 1 ? "it" : "they"} last connected. Restart
        {behind.length === 1 ? "it" : "them"} to apply settings marked “Applies after
        restart”.</Notice
      >{/if}
    <form
      class="trellis-form"
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {#each view.fields as field (field.name)}
        <div class="trellis-field">
          <span class="flex flex-wrap items-center gap-2">
            <label class="trellis-field-label" for={`setting-${field.name}`}
              >{field.title}</label
            >
            {#if field.required}<span
                class={[
                  "badge badge-sm",
                  field.source === "unset" ? "badge-warning" : "badge-ghost",
                ]}>Required</span
              >{/if}
            {#if field.secret}<span
                class="inline-flex items-center gap-1 text-xs text-base-content/60"
                ><Icon name="lock" size={12} />Secret</span
              >{/if}
            {#if field.restart}<span class="badge badge-neutral badge-sm"
                >Applies after restart</span
              >{/if}
            <span class="text-xs text-base-content/60"
              >{sourceLabel(field)}</span
            >
          </span>
          <span class="flex gap-2">
            <label
              class={[
                "input w-full",
                !isText(field) && "font-mono",
                (!editable || busy || !ready) && "input-disabled",
              ]}
              ><input
                id={`setting-${field.name}`}
                class="grow"
                type={field.secret &&
                !(revealed[field.name] && drafts[field.name])
                  ? "password"
                  : "text"}
                autocomplete="off"
                disabled={!editable || busy || !ready}
                placeholder={placeholder(field)}
                value={drafts[field.name] ?? (field.secret ? "" : shown(field))}
                oninput={(event) => {
                  drafts = {
                    ...drafts,
                    [field.name]: event.currentTarget.value,
                  };
                }}
              />{#if field.secret}<button
                  type="button"
                  class="btn btn-ghost btn-xs btn-square -mr-2"
                  aria-label={`${revealed[field.name] ? "Hide" : "Show"} ${field.title}`}
                  aria-pressed={revealed[field.name] ?? false}
                  disabled={!drafts[field.name]}
                  title={drafts[field.name]
                    ? undefined
                    : "A saved secret is never shown; type a new one to see it."}
                  onclick={() => {
                    revealed = {
                      ...revealed,
                      [field.name]: !revealed[field.name],
                    };
                  }}
                  ><Icon
                    name={revealed[field.name] ? "eyeOff" : "eye"}
                    size={16}
                  /></button
                >{/if}</label
            >
            {#if editable && types(field).includes("null") && (field.secret ? field.isSet : field.value !== null)}<button
                type="button"
                class="btn btn-ghost btn-sm whitespace-nowrap"
                disabled={busy || !ready}
                aria-label={`Clear ${field.title}`}
                onclick={() =>
                  void store(field, "null", `${field.title} cleared.`)}
                >Clear</button
              >{/if}
            {#if editable && field.isSet && (!loadedInstanceId || field.source === "instance")}<button
                type="button"
                class="btn btn-ghost btn-sm"
                disabled={busy || !ready}
                aria-label={loadedInstanceId
                  ? `Use the service-wide ${field.title}`
                  : `Reset ${field.title}`}
                onclick={() =>
                  void store(
                    field,
                    "unset",
                    loadedInstanceId
                      ? `${field.title} uses the service-wide value again.`
                      : `${field.title} reset.`,
                  )}>{loadedInstanceId ? "Use service-wide" : "Reset"}</button
              >{/if}
          </span>
          <span class="trellis-field-help">{field.description}</span>
          {#if field.invalid}<span class="text-xs text-warning"
              >The saved value no longer fits this build and is ignored.</span
            >{/if}
        </div>
      {/each}
      {#if editable}<div class="trellis-action-row">
          <button
            class="btn btn-primary"
            type="submit"
            disabled={busy || !changed || !ready}>Save settings</button
          >
        </div>{/if}
    </form>
    {#if revisions.length > 1}
      <details class="text-sm">
        <summary class="cursor-pointer text-base-content/70"
          >Earlier versions</summary
        >
        <ul class="mt-2 divide-y divide-base-300">
          {#each revisions.filter((item) => !item.current) as revision (revision.revision)}
            <li class="flex items-center justify-between gap-3 py-2">
              <span>
                Saved {formatDate(revision.createdAt)}
                <span class="block text-xs text-base-content/60"
                  >Changed {[...revision.changed, ...revision.changedSecrets]
                    .map(
                      (name) =>
                        view?.fields.find((field) => field.name === name)
                          ?.title ?? name,
                    )
                    .join(", ")}</span
                >
              </span>
              {#if editable}<ActionMenu
                  ariaLabel={`Actions for settings saved ${formatDate(revision.createdAt)}`}
                  ><li>
                    <button
                      disabled={busy || !ready}
                      onclick={() => void restore(revision)}>Restore</button
                    >
                  </li>
                  <li>
                    <button
                      class="text-error"
                      disabled={busy || !ready}
                      onclick={() => void remove(revision)}>Delete</button
                    >
                  </li></ActionMenu
                >{/if}
            </li>
          {/each}
        </ul>
      </details>
    {/if}
  </section>
{/if}
<ConfirmationModal bind:this={confirmationModal} />
