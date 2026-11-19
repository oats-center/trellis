<script lang="ts">
  import { apis } from "$lib/trellis-apis";
  import type { JsonValue } from "@oatscenter/trellis/generated";
  import { onMount, untrack } from "svelte";
  import { ulid } from "ulid";
  import { codecs } from "@oatscenter/trellis/generated";
  import { formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { canPerform } from "$lib/console/operations.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { classifyMutationError } from "$lib/console/mutation.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import {
    freeSettings,
    isTextSetting,
    parseSetting,
    settingTypes,
    type FreeSetting,
  } from "$lib/console/settings.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import FreeSettingRows from "$lib/components/FreeSettingRows.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import SettingInput from "$lib/components/SettingInput.svelte";

  type View = apis.auth.ConfigGetOutput;
  type Field = View["fields"][number];
  type Unused = View["unused"][number];
  type Revision = apis.auth.ConfigRevisionsListOutput["items"][number];

  // Settings for a service or device deployment or one of its instances'
  // overrides, for a service added without its IDL, or for an app's
  // registered client. Settings can be added by name for fields no known
  // build declares; each build uses the ones it declares.
  type Props = {
    deploymentId?: string;
    clientId?: string;
    /** A service added without its IDL, before it is approved. */
    claimId?: string;
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
    claimId,
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
  // Settings added by name, saved with the drafts.
  let added = $state<FreeSetting[]>([]);
  // Secrets typed here that the administrator chose to see; a saved secret is
  // never sent back, so only a draft can be shown. A choice lasts only as long
  // as its draft: the next value typed for that field starts masked.
  let revealed = $state<Record<string, boolean>>({});
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let busy = $state(false);
  let confirmationModal: ConfirmationModal | undefined = $state();

  const target = $derived(
    claimId
      ? { claimId }
      : clientId
        ? { clientId }
        : { deploymentId, ...(instanceId ? { instanceId } : {}) },
  );
  const key = $derived(`${claimId ?? clientId ?? deploymentId}:${instanceId}`);
  // The shown settings belong to what the selector names.
  const ready = $derived(view !== null && loadedKey === key);
  const loadedInstanceId = $derived(view?.target.instanceId);
  const editable = $derived(canPerform(authority.authority, "configSet"));
  const changed = $derived(
    Object.keys(drafts).length > 0 || added.some((row) => row.name.trim()),
  );
  // Apps show settings only once they declare some or have some saved; a
  // service or device can always take settings by name.
  const shown = $derived(
    view !== null &&
      (view.fields.length > 0 ||
        view.unused.length > 0 ||
        instanceId !== "" ||
        (editable && !clientId)),
  );
  // Instances whose own build would read different restart-only values if
  // it connected now.
  const behind = $derived(
    (view?.instances ?? []).filter((instance) => instance.restartPending),
  );

  // Drafts belong to one target: choosing another discards them.
  $effect(() => {
    if (!scope.setKey(key)) return;
    drafts = {};
    added = [];
    revealed = {};
  });
  // The settings are reread whenever the page hands over its target again,
  // as the service page does each time it rereads the service. The drafts
  // stay, and fields left alone show what is saved now.
  $effect(() => {
    void target;
    untrack(() => void load());
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
        available = loaded.fields.length > 0 || loaded.unused.length > 0;
      }
      revisions = history.items;
      error = null;
    } catch (cause) {
      if (scope.isCurrent(token)) error = projectConsoleError(cause).message;
    } finally {
      scope.settle(token);
    }
  }

  function shownValue(field: Field): string {
    if (field.value === undefined) return "";
    if (field.value === null && isTextSetting(field)) return "";
    return typeof field.value === "string" && isTextSetting(field)
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

  function unusedValue(item: Unused): string {
    if (item.secret) return "Secret, set";
    return typeof item.value === "string"
      ? item.value
      : JSON.stringify(item.value);
  }

  // Runs a change captured for `intentKey`. Its outcome is shown only while
  // the selector still names that target. On success the drafts of `settled`
  // fields go, or every draft and added row when it is omitted; the rest stay
  // pending.
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
      revealed = settled
        ? Object.fromEntries(
            Object.entries(revealed).filter(
              ([name]) => !settled.includes(name),
            ),
          )
        : {};
      if (!settled) added = [];
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

  // Trellis never restarts anything, so saving a setting that applies after a
  // restart asks first, saying the administrator restarts it themselves.
  async function confirmRestart(fields: Field[]): Promise<boolean> {
    const restart = fields.filter((field) => field.restart);
    if (restart.length === 0) return true;
    const intentKey = loadedKey;
    const names = restart.map((field) => field.title).join(", ");
    const confirmed = await confirmationModal?.confirm({
      title: "Restart to apply",
      message: `${names} ${restart.length === 1 ? "applies" : "apply"} only after a restart. Trellis doesn't restart ${loadedInstanceId ? "the instance" : "the service"}: after saving, restart ${loadedInstanceId ? "it" : "its instances"} yourself to use the new ${restart.length === 1 ? "value" : "values"}.`,
      confirmLabel: "Save",
    });
    return confirmed === true && key === intentKey;
  }

  async function save(): Promise<void> {
    if (!ready || !view) return;
    const current = view;
    let values: Record<string, JsonValue>;
    let secrets: Record<string, JsonValue>;
    try {
      ({ values, secrets } = freeSettings(added));
      for (const [name, text] of Object.entries(drafts)) {
        const field = current.fields.find((item) => item.name === name);
        if (!field) continue;
        if (name in values || name in secrets)
          throw new Error(`${field.title} is also added by name below.`);
        if (field.secret) {
          if (text !== "") secrets[name] = parseSetting(field, text);
        } else values[name] = parseSetting(field, text);
      }
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
      return;
    }
    if (
      !(await confirmRestart(
        current.fields.filter(
          (field) => field.name in values || field.name in secrets,
        ),
      ))
    )
      return;
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
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow(),
      "Settings saved.",
    );
  }

  // Unset removes the value stored here, so the default or deployment
  // applies; null stores null here, overriding them. Only this field's draft
  // is dropped; other unsaved edits stay pending.
  async function store(
    field: { name: string; secret: boolean; restart?: boolean },
    change: "unset" | "null",
    done: string,
  ): Promise<void> {
    if (!ready || !view) return;
    const current = view;
    const declared = current.fields.find((item) => item.name === field.name);
    if (declared && !(await confirmRestart([declared]))) return;
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
            idempotencyKey: codecs.ulid.decode(ulid()),
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
            idempotencyKey: codecs.ulid.decode(ulid()),
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
            idempotencyKey: codecs.ulid.decode(ulid()),
          })
          .orThrow(),
      "Earlier version deleted.",
    );
  }

  onMount(() => () => scope.dispose());
</script>

{#if view && shown}
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
    {#if !view.declared}<p class="text-sm text-base-content/65">
        Trellis doesn't have its IDL yet. Add settings by name; the service
        checks them when it connects.
      </p>{/if}
    <form
      class="trellis-form"
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {#each view.fields as field (field.name)}
        <SettingInput
          {field}
          id={`setting-${field.name}`}
          value={drafts[field.name] ?? (field.secret ? "" : shownValue(field))}
          placeholder={placeholder(field)}
          disabled={!editable || busy || !ready}
          missing={field.source === "unset"}
          revealed={revealed[field.name] ?? false}
          oninput={(text) => {
            drafts = { ...drafts, [field.name]: text };
            // Emptying the field ends the draft that was shown.
            if (!text && revealed[field.name]) {
              revealed = { ...revealed, [field.name]: false };
            }
          }}
          onreveal={() => {
            revealed = { ...revealed, [field.name]: !revealed[field.name] };
          }}
        >
          {#snippet status()}<span class="text-xs text-base-content/60"
              >{sourceLabel(field)}</span
            >{/snippet}
          {#snippet actions()}
            {#if editable && settingTypes(field).includes("null") && (field.secret ? field.isSet : field.value !== null)}<button
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
          {/snippet}
          {#snippet note()}
            {#if field.storedAsPlain}<p class="text-xs text-warning">
                Stored as plain text; set it as a secret. This build never uses
                or shows the plain value. Type the value above and save. The old
                plain value stays in earlier versions until you delete them.
              </p>
            {:else if field.invalid}<details class="text-xs text-warning">
                <summary class="cursor-pointer"
                  >{field.source === "unset"
                    ? "The saved value doesn't fit this build, so it waits for a new one."
                    : "The saved value doesn't fit this build, so it uses the one before it."}</summary
                >
                <p class="mt-1 text-base-content/70">{field.invalid}</p>
              </details>{/if}
          {/snippet}
        </SettingInput>
      {/each}
      {#if view.unused.length > 0}
        <div
          class="trellis-field"
          role="group"
          aria-label="Settings this build doesn't use"
        >
          <span class="trellis-field-label"
            >{view.declared
              ? "Not used by this build"
              : "Saved for when it connects"}</span
          >
          <ul class="divide-y divide-base-300 text-sm">
            {#each view.unused as item (item.name)}
              <li class="flex flex-wrap items-center gap-x-3 gap-y-1 py-2">
                <span class="font-mono">{item.name}</span>
                <span class="min-w-0 flex-1 truncate text-base-content/70"
                  >{unusedValue(item)}</span
                >
                {#if item.source === "deployment" && loadedInstanceId}<span
                    class="text-xs text-base-content/60"
                    >Same as every instance</span
                  >{/if}
                {#if editable && (!loadedInstanceId || item.source === "instance")}<button
                    type="button"
                    class="btn btn-ghost btn-sm"
                    disabled={busy || !ready}
                    aria-label={`Remove ${item.name}`}
                    onclick={() =>
                      void store(item, "unset", `${item.name} removed.`)}
                    >Remove</button
                  >{/if}
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if editable}
        <div class="trellis-field">
          <span class="trellis-field-label">Add by name</span>
          <FreeSettingRows
            bind:rows={added}
            allowSecrets={!clientId}
            disabled={busy || !ready}
          />
        </div>
        <div class="trellis-action-row">
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
