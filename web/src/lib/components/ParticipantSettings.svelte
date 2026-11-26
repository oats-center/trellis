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
    allowsNull,
    formatValue,
    freeSettings,
    hasProblem,
    parseSecret,
    settingSchema,
    toJson,
    type Draft,
    type FreeSetting,
  } from "$lib/console/settings.ts";
  import { sameValue } from "$lib/console/draft.ts";
  import ActionMenu from "$lib/components/ActionMenu.svelte";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DraftConflicts from "$lib/components/DraftConflicts.svelte";
  import FreeSettingRows from "$lib/components/FreeSettingRows.svelte";
  import Icon from "$lib/components/Icon.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import SettingInput from "$lib/components/SettingInput.svelte";
  import StatusBadge from "$lib/components/StatusBadge.svelte";

  type View = apis.auth.ConfigGetOutput;
  type Field = View["fields"][number];
  type Unused = View["unused"][number];
  type Revision = apis.auth.ConfigRevisionsListOutput["items"][number];
  // What was saved for a field when its draft was typed over it.
  type Base = Pick<Field, "value" | "isSet" | "source"> & {
    revision: View["revision"];
  };

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
  // What the administrator entered for the loaded target, by field name: a
  // typed value for a setting, the text typed for a secret. Untouched fields
  // are absent; switching targets discards them.
  let drafts = $state<Record<string, Draft>>({});
  // What each draft was typed over. A reread leaves a draft as typed; when
  // someone saved its field meanwhile, it is a conflict, shown beside the
  // saved value, and nothing is saved until the administrator settles it.
  let bases = $state.raw<Record<string, Base>>({});
  // Settings added by name, saved with the drafts.
  let added = $state<FreeSetting[]>([]);
  // Secrets shown rather than masked: a typed draft, or a saved value Auth
  // revealed on request. A revealed value lives only here, only while shown:
  // hiding it, typing over it, saving, or choosing another target drops it.
  let shown = $state<Record<string, boolean>>({});
  let revealed = $state<Record<string, string>>({});
  let revealing = $state<string | null>(null);
  // A failed read of the settings; the next read clears it.
  let loadError = $state<string | null>(null);
  // The outcome of the administrator's last change. Rereads leave it; it
  // lasts until they act again or choose another target.
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
  // Every settings role reaches the reveal RPC so its refusal can say what is
  // missing; only one that can manage secrets is offered the eye.
  const canReveal = $derived(
    canPerform(authority.authority, "configSecretsReveal") &&
      (authority.authority?.capabilities.some(
        ({ capabilityId }) =>
          capabilityId === "trellis.auth.v1:config.manageSecrets",
      ) ??
        false),
  );
  const changed = $derived(
    Object.keys(drafts).length > 0 || added.some((row) => row.name.trim()),
  );
  // Apps show settings only once they declare some or have some saved; a
  // service or device can always take settings by name.
  const shownSection = $derived(
    view !== null &&
      (view.fields.length > 0 ||
        view.unused.length > 0 ||
        instanceId !== "" ||
        (editable && !clientId)),
  );
  // Whether someone saved `field` since its draft was typed over `base`. A
  // secret's value is never shown, so its versions say whether it changed;
  // when they no longer reach back to `base`, it counts as changed.
  function savedSince(field: Field, base: Base): boolean {
    if (base.isSet !== field.isSet || base.source !== field.source) return true;
    if (!field.secret) return !sameValue(base.value, field.value);
    if (base.revision === view?.revision) return false;
    const covered =
      base.revision === undefined
        ? revisions.length < 20
        : revisions.some((item) => item.revision === base.revision);
    return (
      !covered ||
      revisions.some(
        (item) =>
          (base.revision === undefined || item.revision > base.revision) &&
          item.changedSecrets.includes(field.name),
      )
    );
  }
  function baseOf(field: Field): Base {
    return {
      value: field.value,
      isSet: field.isSet,
      source: field.source,
      revision: view?.revision,
    };
  }
  // Drafts typed over a value someone saved since. A plain draft that
  // already equals the saved value is no conflict.
  const conflicts = $derived(
    ready && view
      ? view.fields.filter((field) => {
          const base = bases[field.name];
          return (
            field.name in drafts &&
            base !== undefined &&
            savedSince(field, base) &&
            (field.secret || !sameValue(drafts[field.name], field.value))
          );
        })
      : [],
  );
  function startDraft(field: Field): void {
    if (!(field.name in drafts))
      bases = { ...bases, [field.name]: baseOf(field) };
  }
  // Keep yours saves the draft over the newer value; use saved drops it.
  function settle(name: string, choice: "keep" | "take"): void {
    const field = view?.fields.find((item) => item.name === name);
    if (!field) return;
    if (choice === "keep") bases = { ...bases, [name]: baseOf(field) };
    else {
      drafts = Object.fromEntries(
        Object.entries(drafts).filter(([other]) => other !== name),
      );
      hide([name]);
    }
  }
  function draftText(draft: Draft): string {
    try {
      return formatValue(toJson(draft));
    } catch {
      return "(incomplete)";
    }
  }

  // Instances whose own build would read different restart-only values if
  // it connected now.
  const behind = $derived(
    (view?.instances ?? []).filter((instance) => instance.restartPending),
  );
  // What each instance did with the newest settings, for instances whose
  // build declares any.
  const applying = $derived(
    (view?.instances ?? []).flatMap((instance) =>
      instance.settings ? [{ instance, settings: instance.settings }] : [],
    ),
  );
  const refusing = $derived(
    applying.filter(({ settings }) => settings.state === "rejected"),
  );
  function titlesOf(names: readonly string[]): string {
    return names
      .map(
        (name) =>
          view?.fields.find((field) => field.name === name)?.title ?? name,
      )
      .join(", ");
  }

  // Drafts belong to one target: choosing another discards them.
  $effect(() => {
    if (!scope.setKey(key)) return;
    drafts = {};
    bases = {};
    added = [];
    shown = {};
    revealed = {};
    error = null;
    notice = null;
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
      loadError = null;
      // A draft whose field nobody saved meanwhile moves onto this read;
      // the others stay conflicts until the administrator settles them.
      bases = Object.fromEntries(
        Object.entries(bases).map(([name, base]) => {
          const field = loaded.fields.find((item) => item.name === name);
          const moves =
            field !== undefined &&
            (!savedSince(field, base) ||
              (!field.secret && sameValue(drafts[name], field.value)));
          return [name, moves ? baseOf(field) : base];
        }),
      );
    } catch (cause) {
      if (scope.isCurrent(token))
        loadError = projectConsoleError(cause).message;
    } finally {
      scope.settle(token);
    }
  }

  function sourceLabel(field: Field): string {
    switch (field.source) {
      case "instance":
        return "Set for this instance";
      case "deployment":
        return loadedInstanceId ? "Same as every instance" : "Set";
      case "default":
        return "Using the default";
      default:
        return "Not set";
    }
  }

  // Drops what is shown of `names`' secrets, or of every secret.
  function hide(names?: readonly string[]): void {
    const keep = ([name]: [string, unknown]) =>
      names !== undefined && !names.includes(name);
    shown = Object.fromEntries(Object.entries(shown).filter(keep));
    revealed = Object.fromEntries(Object.entries(revealed).filter(keep));
  }

  // Shows a typed secret, or asks Auth for the saved one, which it records.
  async function toggleSecret(name: string): Promise<void> {
    if (shown[name]) {
      hide([name]);
      return;
    }
    const draft = drafts[name];
    if (typeof draft === "string" && draft !== "") {
      shown = { ...shown, [name]: true };
      return;
    }
    if (!ready || !view || revealing) return;
    const intentKey = loadedKey;
    revealing = name;
    error = null;
    notice = null;
    try {
      const secret = await trellis
        .configSecretsReveal({ target: view.target, field: name })
        .orThrow();
      if (key !== intentKey) return;
      revealed = { ...revealed, [name]: formatValue(secret.value) };
      shown = { ...shown, [name]: true };
    } catch (cause) {
      if (key === intentKey) error = projectConsoleError(cause).message;
    } finally {
      revealing = null;
    }
  }

  // Typing over a secret replaces what was shown of it; emptying the field
  // ends the draft and masks it again.
  function typeSecret(name: string, text: string): void {
    if (text === "") {
      drafts = Object.fromEntries(
        Object.entries(drafts).filter(([other]) => other !== name),
      );
      hide([name]);
      return;
    }
    drafts = { ...drafts, [name]: text };
    revealed = Object.fromEntries(
      Object.entries(revealed).filter(([other]) => other !== name),
    );
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
      // What was typed is saved now, and a secret goes back to masked.
      drafts = settled
        ? Object.fromEntries(
            Object.entries(drafts).filter(([name]) => !settled.includes(name)),
          )
        : {};
      hide(settled);
      if (!settled) added = [];
      notice = done;
      await load();
    } catch (cause) {
      if (key !== intentKey) return;
      const refused = classifyMutationError(cause).kind !== "unknown";
      error = refused
        ? projectConsoleError(cause).message
        : "We couldn't confirm the change. Refresh before trying again.";
      // A refusal means the settings changed since they were read; show them
      // as they are now. The drafts stay, so nothing typed is lost.
      if (refused) await load();
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
    if (!ready || !view || conflicts.length > 0) return;
    const current = view;
    let values: Record<string, JsonValue>;
    let secrets: Record<string, JsonValue>;
    try {
      ({ values, secrets } = freeSettings(added));
      const unfit: string[] = [];
      for (const [name, draft] of Object.entries(drafts)) {
        const field = current.fields.find((item) => item.name === name);
        if (!field) continue;
        if (name in values || name in secrets)
          throw new Error(`${field.title} is also added by name below.`);
        if (field.secret) {
          if (typeof draft === "string" && draft !== "")
            secrets[name] = parseSecret(field, draft);
          continue;
        }
        const { root, defs } = settingSchema(field);
        if (hasProblem(root, defs, draft)) unfit.push(field.title);
        else values[name] = toJson(draft);
      }
      if (unfit.length > 0)
        throw new Error(
          `Fix ${unfit.join(", ")} before saving. Nothing was saved.`,
        );
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
      notice = null;
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

  // An unused value is kept for a build that declares it, so removing one
  // asks first.
  async function removeUnused(item: Unused): Promise<void> {
    if (!ready) return;
    const intentKey = loadedKey;
    const confirmed = await confirmationModal?.confirm({
      title: `Remove ${item.name}?`,
      message: item.secret
        ? "The secret goes from the current settings. Earlier versions keep it until you delete them."
        : `A later build that uses ${item.name} would start without it.`,
      confirmLabel: "Remove",
    });
    if (!confirmed || key !== intentKey) return;
    await store(item, "unset", `${item.name} removed.`);
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

{#snippet eye(name: string, title: string, isSet: boolean)}
  {#if revealing === name}<span
      class="loading loading-spinner loading-xs"
      aria-label={`Getting ${title}`}
    ></span>{:else}<button
      type="button"
      class="btn btn-ghost btn-xs btn-square"
      aria-label={`${shown[name] ? "Hide" : "Show"} ${title}`}
      aria-pressed={shown[name] ?? false}
      disabled={!ready || (!shown[name] && (!isSet || !canReveal))}
      title={shown[name] || (isSet && canReveal)
        ? undefined
        : isSet
          ? "Only administrators who manage secrets can see them."
          : "Nothing is saved yet."}
      onclick={() => void toggleSecret(name)}
      ><Icon name={shown[name] ? "eyeOff" : "eye"} size={16} /></button
    >{/if}
{/snippet}

{#if view && shownSection}
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
            aria-label="Settings for"
            bind:value={instanceId}
            ><option value="">Every instance</option
            >{#each instances as instance (instance.instanceId)}<option
                value={instance.instanceId}>{nameOf(instance)}</option
              >{/each}</select
          >{#if !ready && !loadError}<span
              class="loading loading-spinner loading-xs"
              aria-label="Loading settings"
            ></span>{/if}</label
        >
      {/if}
    </div>
    {#if notice}<Notice variant="success">{notice}</Notice>{/if}
    {#if error}<Notice variant="error">{error}</Notice>{/if}
    {#if loadError}<Notice variant="warning">{loadError}</Notice>{/if}
    {#if behind.length > 0}<Notice variant="info"
        >{behind.map(nameOf).join(", ")}
        {behind.length === 1 ? "uses" : "use"} the settings from when
        {behind.length === 1 ? "it" : "they"} last connected. Restart
        {behind.length === 1 ? "it" : "them"} to apply settings marked “Applies after
        restart”.</Notice
      >{/if}
    {#each refusing as { instance, settings } (instance.instanceId)}
      <!-- A fixed value for one of these reaches it only when it restarts. -->
      {@const afterRestart = settings.fields.filter(
        (name) => view?.fields.find((field) => field.name === name)?.restart,
      )}<Notice variant="warning"
        >{nameOf(instance)} rejected the latest settings{settings.fields
          .length > 0
          ? ` for ${titlesOf(settings.fields)}`
          : ""}: {settings.reason}. It keeps using the settings it had. Fix them
        below or restore an earlier version.{afterRestart.length > 0
          ? ` ${titlesOf(afterRestart)} ${afterRestart.length === 1 ? "applies" : "apply"} after restart, so restart it once fixed.`
          : ""}</Notice
      >{/each}
    {#if applying.length > 0}
      <ul class="grid gap-1 text-sm" aria-label="Instances">
        {#each applying as { instance, settings } (instance.instanceId)}
          <!-- An instance that applied the latest settings but still runs
          older values that apply after a restart uses only part of them. -->
          {@const partly =
            settings.state === "applied" && instance.restartPending}
          <li class="flex flex-wrap items-center gap-x-2">
            <StatusBadge
              dot
              label={partly
                ? "Restart needed"
                : settings.state === "applied"
                  ? "Applied"
                  : settings.state === "rejected"
                    ? "Rejected"
                    : "Waiting"}
              status={partly
                ? "degraded"
                : settings.state === "applied"
                  ? "healthy"
                  : settings.state === "rejected"
                    ? "degraded"
                    : "offline"}
            />
            <span>{nameOf(instance)}</span>
            <span class="text-base-content/65"
              >{partly
                ? "Uses the latest settings except those that apply after restart"
                : settings.state === "applied"
                  ? "Uses the latest settings"
                  : settings.state === "rejected"
                    ? "Rejected the latest settings"
                    : "Hasn't read the latest settings yet"}</span
            >
          </li>
        {/each}
      </ul>
    {/if}
    {#if !view.declared}<p class="text-sm text-base-content/65">
        Trellis doesn't have its IDL yet. Add settings by name; the service
        checks them when it connects.
      </p>{/if}
    <form
      class="grid gap-6"
      novalidate
      onsubmit={(event) => {
        event.preventDefault();
        void save();
      }}
    >
      {#if view.fields.length > 0}
        <div class="divide-y divide-base-300">
          {#each view.fields as field (field.name)}
            {@const draft = drafts[field.name]}
            <SettingInput
              {field}
              id={`setting-${field.name}`}
              value={field.secret
                ? (draft ??
                  (shown[field.name] ? revealed[field.name] : undefined) ??
                  "")
                : field.name in drafts
                  ? draft
                  : field.value}
              edited={field.name in drafts}
              placeholder={field.value === null ? "No value" : ""}
              disabled={!editable || busy || !ready}
              missing={field.source === "unset"}
              onchange={(next) => {
                startDraft(field);
                if (field.secret)
                  typeSecret(field.name, typeof next === "string" ? next : "");
                else drafts = { ...drafts, [field.name]: next };
              }}
              secretSet={field.isSet}
              secretShown={shown[field.name] ?? false}
              revealing={revealing === field.name}
              onreveal={() => void toggleSecret(field.name)}
              revealBlocked={field.isSet && !canReveal
                ? "Only administrators who manage secrets can see them."
                : !field.isSet
                  ? "Nothing is saved yet."
                  : undefined}
            >
              {#snippet status()}<span>{sourceLabel(field)}</span
                >{#if field.name in drafts}<span
                    class="font-medium text-base-content/80"
                    >Changed, not saved</span
                  >{/if}{/snippet}
              {#snippet actions()}
                {#if editable && field.name in drafts && !field.secret}<button
                    type="button"
                    class="btn btn-ghost btn-sm font-normal"
                    disabled={busy}
                    aria-label={`Undo changes to ${field.title}`}
                    onclick={() => {
                      drafts = Object.fromEntries(
                        Object.entries(drafts).filter(
                          ([name]) => name !== field.name,
                        ),
                      );
                    }}>Undo</button
                  >{/if}
                {#if editable && allowsNull(field) && (field.secret ? field.isSet : field.value !== null)}<button
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
                    class="btn btn-ghost btn-sm whitespace-nowrap"
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
                      )}
                    >{loadedInstanceId ? "Use service-wide" : "Reset"}</button
                  >{/if}
              {/snippet}
              {#snippet note()}
                {#if field.storedAsPlain}<p class="text-xs text-warning">
                    Stored as plain text; set it as a secret. This build never
                    uses or shows the plain value. Type the value above and
                    save. The old plain value stays in earlier versions until
                    you delete them.
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
        </div>
      {/if}
      {#if view.unused.length > 0}
        <div
          role="group"
          class="grid gap-2 rounded-box border border-base-300 p-4"
          aria-label="Settings this build doesn't use"
        >
          <div>
            <h3 class="text-sm font-semibold">
              {view.declared
                ? "Not used by this build"
                : "Saved for when it connects"}
            </h3>
            <p class="text-xs text-base-content/60">
              {view.declared
                ? "Saved, but the installed build doesn't declare them, so it ignores them. A build that declares them uses them."
                : "The service's IDL decides which of these it uses."}
            </p>
          </div>
          <ul class="divide-y divide-base-300 text-sm">
            {#each view.unused as item (item.name)}
              <li
                class="flex flex-wrap items-center gap-x-3 gap-y-1 py-2"
                aria-label={item.name}
              >
                <span class="font-mono font-medium">{item.name}</span>
                {#if item.secret}
                  <span
                    class="flex min-w-0 flex-1 items-center gap-1.5 text-base-content/70"
                    ><Icon name="lock" size={12} /><span
                      class={[
                        "min-w-0 truncate",
                        shown[item.name] && "font-mono",
                      ]}
                      >{shown[item.name] && revealed[item.name] !== undefined
                        ? revealed[item.name]
                        : "Secret, set"}</span
                    >{@render eye(item.name, item.name, true)}</span
                  >
                {:else}
                  <span
                    class="min-w-0 flex-1 truncate font-mono text-base-content/70"
                    >{item.value === undefined
                      ? ""
                      : formatValue(item.value)}</span
                  >
                {/if}
                {#if item.source === "deployment" && loadedInstanceId}<span
                    class="text-xs text-base-content/60"
                    >Same as every instance</span
                  >{/if}
                {#if editable && (!loadedInstanceId || item.source === "instance")}<button
                    type="button"
                    class="btn btn-ghost btn-sm text-error"
                    disabled={busy || !ready}
                    aria-label={`Remove ${item.name}`}
                    onclick={() => void removeUnused(item)}>Remove</button
                  >{/if}
              </li>
            {/each}
          </ul>
        </div>
      {/if}
      {#if editable}
        <div class="grid gap-2" role="group" aria-label="Add by name">
          <div>
            <h3 class="text-sm font-semibold">Add by name</h3>
            <p class="text-xs text-base-content/60">
              For settings no installed build declares yet.
            </p>
          </div>
          <FreeSettingRows
            bind:rows={added}
            allowSecrets={!clientId}
            disabled={busy || !ready}
          />
        </div>
        <DraftConflicts
          conflicts={conflicts.map((field) => ({
            field: field.name,
            label: field.title,
            mine: field.secret
              ? "A new value (hidden)"
              : draftText(drafts[field.name]),
            saved: field.secret
              ? field.isSet
                ? "Changed (hidden)"
                : "Not set"
              : field.value === undefined
                ? "Not set"
                : formatValue(field.value),
          }))}
          disabled={busy}
          onsettle={settle}
        />
        <div class="trellis-action-row">
          <button
            class="btn btn-primary"
            type="submit"
            disabled={busy || !changed || !ready || conflicts.length > 0}
            >Save settings</button
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
