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
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import Notice from "$lib/components/Notice.svelte";

  type View = apis.auth.ConfigGetOutput;
  type Field = View["fields"][number];
  type Revision = apis.auth.ConfigRevisionsListOutput["items"][number];

  // Settings a service or device declares, for the deployment or one of its
  // instances' overrides.
  // A service or device deployment, or an app's registered client.
  let { deploymentId, clientId }: { deploymentId?: string; clientId?: string } =
    $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("participant-settings");
  let instanceId = $state("");
  let view = $state.raw<View | null>(null);
  // Every instance of the deployment, for choosing whose overrides to edit.
  let instances = $state.raw<View["instances"]>([]);
  let revisions = $state.raw<Revision[]>([]);
  // What the administrator typed, by field name. Untouched fields are absent.
  let drafts = $state<Record<string, string>>({});
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const target = $derived(
    clientId
      ? { clientId }
      : { deploymentId, ...(instanceId ? { instanceId } : {}) },
  );
  const editable = $derived(canPerform(authority.authority, "configSet"));
  const changed = $derived(Object.keys(drafts).length > 0);
  // Instances still running restart-only values from an earlier save.
  const behind = $derived.by(() => {
    const current = view;
    if (!current?.fields.some((field) => field.restart)) return [];
    return current.instances.filter((instance) =>
      instanceId
        ? instance.connectedRevision !== current.deploymentRevision ||
          instance.connectedInstanceRevision !== current.revision
        : instance.connectedRevision !== current.revision,
    );
  });

  $effect(() => {
    scope.setKey(`${clientId ?? deploymentId}:${instanceId}`);
    void load();
  });

  async function load(): Promise<void> {
    const token = scope.begin();
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
      if (!instanceId) instances = loaded.instances;
      revisions = history.items;
      drafts = {};
      error = null;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      scope.settle(token);
    }
  }

  function schemaOf(field: Field): Record<string, unknown> {
    const schema = field.schema as Record<string, unknown>;
    const ref = typeof schema.$ref === "string" ? schema.$ref : undefined;
    const defs = schema.$defs as
      Record<string, Record<string, unknown>> | undefined;
    return ref && defs ? (defs[ref.replace("#/$defs/", "")] ?? schema) : schema;
  }

  // Text settings are typed as text; anything else as JSON.
  function isText(field: Field): boolean {
    return schemaOf(field).type === "string";
  }

  function shown(field: Field): string {
    if (field.value === undefined) return "";
    return typeof field.value === "string" && isText(field)
      ? field.value
      : JSON.stringify(field.value);
  }

  function sourceLabel(field: Field): string {
    switch (field.source) {
      case "instance":
        return "Set for this instance";
      case "deployment":
        return instanceId ? "From the service" : "Set";
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

  async function mutate(run: () => Promise<View>, done: string): Promise<void> {
    if (busy) return;
    busy = true;
    error = null;
    notice = null;
    try {
      view = await run();
      // A saved secret is never shown back, so what was typed goes at once.
      drafts = {};
      notice = done;
      await load();
    } catch (cause) {
      error =
        classifyMutationError(cause).kind === "unknown"
          ? "We couldn't confirm the change. Refresh before trying again."
          : projectConsoleError(cause).message;
    } finally {
      busy = false;
    }
  }

  async function save(): Promise<void> {
    if (!view) return;
    const values: Record<string, JsonValue> = {};
    const secrets: Record<string, JsonValue> = {};
    try {
      for (const [name, text] of Object.entries(drafts)) {
        const field = view.fields.find((item) => item.name === name);
        if (!field) continue;
        if (field.secret) {
          if (text !== "") secrets[name] = parse(field, text);
        } else values[name] = parse(field, text);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
      return;
    }
    const revision = view.revision;
    await mutate(
      () =>
        trellis
          .configSet({
            target: view!.target,
            values,
            secrets,
            ...(revision === undefined ? {} : { expectedRevision: revision }),
            idempotencyKey: ulid(),
          })
          .orThrow(),
      "Settings saved.",
    );
  }

  async function clear(field: Field): Promise<void> {
    if (!view) return;
    const revision = view.revision;
    await mutate(
      () =>
        trellis
          .configSet({
            target: view!.target,
            unset: [field.name],
            ...(revision === undefined ? {} : { expectedRevision: revision }),
            idempotencyKey: ulid(),
          })
          .orThrow(),
      `${field.title} cleared.`,
    );
  }

  async function restore(revision: Revision): Promise<void> {
    if (!view?.revision) return;
    const confirmed = await confirmationModal?.confirm({
      title: "Restore these settings?",
      message: `The settings saved ${formatDate(revision.createdAt)} become current again.`,
      confirmLabel: "Restore",
    });
    if (!confirmed) return;
    const expectedRevision = view.revision;
    await mutate(
      () =>
        trellis
          .configRollback({
            target: view!.target,
            revision: revision.revision,
            expectedRevision,
            idempotencyKey: ulid(),
          })
          .orThrow(),
      "Earlier settings restored.",
    );
  }

  async function remove(revision: Revision): Promise<void> {
    const confirmed = await confirmationModal?.confirm({
      title: "Delete this earlier version?",
      message:
        "It can't be restored afterwards. Delete versions that hold replaced passwords or keys.",
      confirmLabel: "Delete version",
    });
    if (!confirmed || !view) return;
    await mutate(
      () =>
        trellis
          .configRevisionsDelete({
            target: view!.target,
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
      {#if instances.length > 1 || instanceId}
        <label class="flex items-center gap-2 text-sm"
          ><span class="text-base-content/65">For</span><select
            class="select select-sm"
            bind:value={instanceId}
            ><option value="">Every instance</option
            >{#each instances as instance (instance.instanceId)}<option
                value={instance.instanceId}>{instance.title}</option
              >{/each}</select
          ></label
        >
      {/if}
    </div>
    {#if notice}<Notice variant="success">{notice}</Notice>{/if}
    {#if error}<Notice variant="error">{error}</Notice>{/if}
    {#if view.missing.length > 0}<Notice variant="warning"
        >Its instances wait until {view.missing.length === 1
          ? "this is"
          : "these are"} set: {view.fields
          .filter((field) => view?.missing.includes(field.name))
          .map((field) => field.title)
          .join(", ")}.</Notice
      >{/if}
    {#if behind.length > 0}<Notice variant="info"
        >{behind.map((instance) => instance.title).join(", ")}
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
            {#if field.required && field.source === "unset"}<span
                class="badge badge-warning badge-sm">Required</span
              >{/if}
            {#if field.restart}<span class="badge badge-neutral badge-sm"
                >Applies after restart</span
              >{/if}
            <span class="text-xs text-base-content/60"
              >{sourceLabel(field)}</span
            >
          </span>
          <span class="flex gap-2">
            <input
              id={`setting-${field.name}`}
              class={["input w-full", !isText(field) && "font-mono"]}
              type={field.secret ? "password" : "text"}
              autocomplete="off"
              disabled={!editable || busy}
              placeholder={field.secret
                ? field.isSet
                  ? "Set — type to replace"
                  : "Not set"
                : field.default !== undefined
                  ? `Default: ${typeof field.default === "string" ? field.default : JSON.stringify(field.default)}`
                  : ""}
              value={drafts[field.name] ?? (field.secret ? "" : shown(field))}
              oninput={(event) => {
                drafts = {
                  ...drafts,
                  [field.name]: event.currentTarget.value,
                };
              }}
            />
            {#if editable && field.isSet && (!instanceId || field.source === "instance")}<button
                type="button"
                class="btn btn-ghost btn-sm"
                disabled={busy}
                onclick={() => void clear(field)}>Clear</button
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
            disabled={busy || !changed}>Save settings</button
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
                      disabled={busy}
                      onclick={() => void restore(revision)}>Restore</button
                    >
                  </li>
                  <li>
                    <button
                      class="text-error"
                      disabled={busy}
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
