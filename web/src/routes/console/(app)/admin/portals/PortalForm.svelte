<script lang="ts">
  import { ulid } from "ulid";
  import { isErr } from "@oats-center/result";
  import { type apis } from "trellis-web-generated";
  import { resolve } from "$lib/console_paths";
  import { goto } from "$app/navigation";
  import { onMount } from "svelte";
  import { catalogPage } from "$lib/console/paging.ts";
  import { RequestScope } from "$lib/console/request_scope.ts";
  import { projectConsoleError } from "$lib/console/display_value.ts";
  import { getConsoleAuthority } from "$lib/console/authority.svelte.ts";
  import { captureIntent, MutationController, type MutationIntent } from "$lib/console/mutation.ts";
  import {
    adoptBackgroundRead,
    adoptExactLoad,
    draftIsDirty,
    fieldsFromPortal,
    type PortalDraftBaseline,
    type PortalDraftFields,
    recordConflict as recordConflictState,
    recordOwnWrite,
    recordReload,
  } from "$lib/console/portal_draft.ts";
  import ConfirmationModal from "$lib/components/ConfirmationModal.svelte";
  import DataTable from "$lib/components/DataTable.svelte";
  import EmptyState from "$lib/components/EmptyState.svelte";
  import LoadingState from "$lib/components/LoadingState.svelte";
  import Notice from "$lib/components/Notice.svelte";
  import Panel from "$lib/components/Panel.svelte";
  import { errorMessage, formatDate } from "$lib/format";
  import { getTrellis } from "$lib/trellis";

  type Portal = apis.auth.PortalsGetOutput["portal"];
  type Route = apis.auth.PortalsGetOutput["routes"][number];

  let { mode, targetPortalId = null }: {
    mode: "create" | "edit";
    targetPortalId?: string | null;
  } = $props();

  const trellis = getTrellis();
  const authority = getConsoleAuthority();
  const scope = new RequestScope("portal");
  const portalMutation = new MutationController<apis.auth.PortalsPutInput, apis.auth.PortalsPutOutput>();
  const settingsMutation = new MutationController<apis.auth.PortalsLoginSettingsUpdateInput, apis.auth.PortalsLoginSettingsUpdateOutput>();
  const routeMutation = new MutationController<apis.auth.PortalsRoutesPutInput, apis.auth.PortalsRoutesPutOutput>();
  const removal = new MutationController<apis.auth.PortalsRoutesRemoveInput, apis.auth.PortalsRoutesRemoveOutput>();

  let loading = $state(true);
  let error = $state<{ message: string; code?: string; id?: string } | null>(null);
  let saving = $state(false);
  let routeBusy = $state(false);
  let uncertain = $state(false);
  let saved = $state<string | null>(null);
  let portal = $state.raw<Portal | null>(null);
  let routes = $state.raw<Route[]>([]);
  /** True when the backend reported the requested portal as unavailable. */
  let notFound = $state(false);

  // One custom-portal Save is exactly one `Portals.Put`; these fields are the
  // real request fields, not shadow copies of removed legacy controls.
  let portalId = $state("");
  let displayName = $state("");
  let entryUrl = $state("");
  let disabled = $state(false);
  let localLogin = $state(true);
  let localRegistration = $state(true);
  let federatedRegistration = $state(false);
  /** `null` preserves the server's configured provider IDs on update. */
  let providers = $state<string[] | null>(null);
  let providersEdited = $state(false);
  let providerDraft = $state("");
  /**
   * The version the editable portal fields were authored against. A portal or
   * settings write sends this version; a background read never advances it
   * while the draft is dirty.
   */
  let draftBaseline = $state.raw<PortalDraftBaseline | null>(null);
  /** True when a background read saw a different version than the baseline. */
  let newerRemote = $state(false);
  /**
   * True when a save was rejected as stale. A conflicted draft keeps its
   * original baseline and is not saved again until the operator explicitly
   * discards and reloads.
   */
  let conflicted = $state(false);
  const expectedVersion = $derived(draftBaseline?.version ?? null);

  // Route editor: current selectors only, one in-place `Routes.Put`.
  let editingRouteId = $state<string | null>(null);
  let routeExpectedVersion = $state<bigint | null>(null);
  let routeParticipantId = $state("");
  let routeDeploymentId = $state("");
  let routeOrigin = $state("");
  let routePriority = $state("0");
  let confirmationModal: ConfirmationModal | undefined = $state();

  const editingExisting = $derived(mode === "edit");
  const builtIn = $derived(portal?.builtIn === true);
  const busy = $derived(loading || saving || routeBusy || uncertain);
  /** True while the editable fields differ from their CAS baseline. */
  const draftDirty = $derived(draftIsDirty(draftBaseline, currentDraftFields()));
  /**
   * A stale write is not an own write. Until the operator explicitly discards
   * the draft, no ordinary save may acquire another writer's version.
   */
  const conflictBlocked = $derived(conflicted);
  const sortedRoutes = $derived.by(() =>
    [...routes].sort((left, right) => {
      const priority = BigInt(left.priority) - BigInt(right.priority);
      if (priority !== 0n) return priority > 0n ? 1 : -1;
      return left.routeId.localeCompare(right.routeId);
    })
  );

  /** The editable portal fields, as the pure draft rules read them. */
  function currentDraftFields(): PortalDraftFields {
    return {
      displayName,
      entryUrl,
      disabled,
      localLogin,
      localRegistration,
      federatedRegistration,
      providers,
      providersEdited,
    };
  }

  /** Applies a field set to the page's editable state. */
  function applyDraftFields(fields: PortalDraftFields): void {
    displayName = fields.displayName;
    entryUrl = fields.entryUrl;
    disabled = fields.disabled;
    localLogin = fields.localLogin;
    localRegistration = fields.localRegistration;
    federatedRegistration = fields.federatedRegistration;
    providers = fields.providers === null ? null : [...fields.providers];
    providersEdited = fields.providersEdited;
    providerDraft = "";
  }

  /** The subset of a real portal record the draft rules read. */
  function draftSourceOf(response: Portal): Parameters<typeof fieldsFromPortal>[0] {
    return {
      version: response.version,
      displayName: response.displayName,
      entryUrl: response.entryUrl ?? null,
      disabled: response.disabled,
      localLogin: response.loginSettings.localLogin,
      localRegistration: response.loginSettings.localRegistration,
      federatedRegistration: response.loginSettings.federatedRegistration,
      // A portal read always carries a concrete configured list; the generated
      // wire type is nullable because the update request uses null to mean
      // "preserve".
      providers: response.loginSettings.providers ?? [],
    };
  }

  function failureFrom(cause: unknown): { message: string; code?: string; id?: string } {
    const projected = projectConsoleError(cause);
    return {
      message: projected.message,
      ...(projected.code === undefined ? {} : { code: projected.code }),
      ...(projected.id === undefined ? {} : { id: projected.id }),
    };
  }

  /**
   * First successful exact load for a target: baseline, draft, and remote all
   * come from that record.
   */
  function applyPortal(response: Portal, loadedRoutes: readonly Route[]): void {
    portal = response;
    routes = [...loadedRoutes];
    portalId = response.portalId;
    const loaded = adoptExactLoad(draftSourceOf(response));
    draftBaseline = loaded.baseline;
    applyDraftFields(loaded.fields);
    newerRemote = false;
  }

  /**
   * A read that is not this form's own successful write.
   *
   * The remote record and routes always update. The editable draft and its CAS
   * baseline adopt the read only while the draft is clean; a dirty draft keeps
   * its original version so the next save reports a conflict rather than
   * silently overwriting a change the operator never reviewed.
   */
  function adoptRemote(
    remote: Portal,
    loadedRoutes: readonly Route[],
    background: boolean,
  ): void {
    portal = remote;
    routes = [...loadedRoutes];
    portalId = remote.portalId;
    if (!background) {
      const loaded = adoptExactLoad(draftSourceOf(remote));
      draftBaseline = loaded.baseline;
      applyDraftFields(loaded.fields);
      newerRemote = false;
      return;
    }
    const result = adoptBackgroundRead(
      draftBaseline,
      currentDraftFields(),
      draftSourceOf(remote),
    );
    draftBaseline = result.baseline;
    newerRemote = result.newerRemote;
    if (result.adopted) applyDraftFields(result.fields);
  }

  /**
   * A stale-version conflict.
   *
   * A rejected write is not an own write: recording the conflict keeps both
   * the draft and its original baseline unchanged, and blocks ordinary saves
   * until the operator explicitly discards and reloads. The optional exact
   * read only refreshes remote context; it never supplies a new version for
   * the retained draft.
   */
  async function recordConflict(portalIdToRead: string): Promise<void> {
    const recorded = recordConflictState(draftBaseline);
    draftBaseline = recorded.baseline;
    conflicted = recorded.conflicted;
    error = {
      message:
        "Another operator changed this portal since you loaded it. Your edits are kept; use Discard edits and reload current portal to review the current values, then reapply the changes you intended.",
    };
    saved = null;
    const current = await trellis.portalsGet({ portalId: portalIdToRead }).take();
    if (scope.disposed) return;
    if (isErr(current)) return;
    portal = current.portal;
    routes = [...current.routes];
  }

  /**
   * Explicit operator reload/discard after a conflict.
   *
   * This is the only way a stale draft may adopt a newer version. The exact
   * read must succeed before the draft is replaced: on failure both the draft
   * and the conflict state survive so the operator can retry.
   */
  async function reloadPortal(): Promise<void> {
    const portalIdToRead = editingExisting ? portalId : portal?.portalId;
    if (portalIdToRead === undefined || portalIdToRead === "") return;
    if (draftDirty || conflicted) {
      const confirmed = await confirmationModal?.confirm({
        title: "Discard edits and reload?",
        message:
          "This discards your unsaved portal edits and loads the current server record and version. Reapply any changes you still want afterwards.",
        confirmLabel: "Discard and reload",
        targetLabel: "Portal",
        targetName: portalIdToRead,
      });
      if (!confirmed) return;
    }
    const token = scope.begin();
    saving = true;
    try {
      const response = await trellis.portalsGet({ portalId: portalIdToRead }).take();
      if (!scope.isCurrent(token)) return;
      const recorded = recordReload(
        { baseline: draftBaseline, conflicted },
        isErr(response)
          ? { ok: false }
          : { ok: true, portal: draftSourceOf(response.portal) },
      );
      if (isErr(response)) {
        draftBaseline = recorded.baseline;
        conflicted = recorded.conflicted;
        error = failureFrom(response);
        return;
      }
      applyPortal(response.portal, response.routes);
      draftBaseline = recorded.baseline;
      conflicted = recorded.conflicted;
      error = null;
      saved = null;
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
    } finally {
      if (scope.settle(token)) saving = false;
    }
  }

  async function load(): Promise<void> {
    const token = scope.begin();
    const requested = targetPortalId ?? "";
    loading = true;
    error = null;
    notFound = false;
    try {
      if (!editingExisting) {
        // A new portal starts from explicit defaults, not a previous draft.
        portal = null;
        routes = [];
        draftBaseline = null;
        newerRemote = false;
        providers = null;
        providersEdited = false;
        return;
      }
      if (requested === "") {
        error = { message: "Portal ID is required." };
        notFound = true;
        return;
      }
      const response = await trellis.portalsGet({ portalId: requested }).take();
      if (!scope.isCurrent(token)) return;
      if (isErr(response)) {
        error = failureFrom(response);
        notFound = error.code === "not_found";
        portal = null;
        return;
      }
      applyPortal(response.portal, response.routes);
    } catch (cause) {
      if (!scope.isCurrent(token)) return;
      error = failureFrom(cause);
      notFound = error.code === "not_found";
    } finally {
      if (scope.settle(token)) loading = false;
    }
  }

  function providersForRequest(): string[] | null {
    // Untouched providers preserve the server's configured list; an explicit
    // edit sends the exact list, including an empty one.
    return providersEdited ? [...(providers ?? [])] : null;
  }

  function addProviderId(): void {
    const candidate = providerDraft.trim();
    if (candidate === "") return;
    providers = [...(providers ?? []), candidate];
    providersEdited = true;
    providerDraft = "";
  }

  function removeProviderId(providerId: string): void {
    providers = (providers ?? []).filter((value) => value !== providerId);
    providersEdited = true;
  }

  function clearProviders(): void {
    providers = [];
    providersEdited = true;
  }

  async function save(event: SubmitEvent | undefined): Promise<void> {
    event?.preventDefault();
    if (busy) return;
    if (conflictBlocked) {
      error = {
        message:
          "This draft conflicts with a newer server version. Use Discard edits and reload current portal before saving again, or your changes will not be applied.",
      };
      return;
    }
    const target = editingExisting ? portalId : portalId.trim();
    if (!editingExisting && target === "") {
      error = { message: "Portal ID is required." };
      return;
    }
    if (displayName.trim() === "") {
      error = { message: "Display name is required." };
      return;
    }
    if (editingExisting && expectedVersion === null) {
      error = { message: "The portal has not finished loading." };
      return;
    }
    const key = ulid();
    const intent = captureIntent<apis.auth.PortalsPutInput>({
      operation: "portalsPut", targetId: target, label: displayName.trim(),
      idempotencyKey: key,
      input: {
        portalId: target,
        displayName: displayName.trim(),
        entryUrl: entryUrl.trim() === "" ? null : entryUrl.trim(),
        disabled,
        expectedVersion,
        idempotencyKey: key,
        loginSettings: {
          localLogin, localRegistration, federatedRegistration,
          providers: providersForRequest(),
        },
      },
      scope: { routeKey: targetPortalId ?? "portal-new" },
    });
    if (!portalMutation.begin(intent)) return;
    saving = true;
    if (editingExisting && portal !== null && !portal.disabled && disabled) {
      const confirmed = await confirmationModal?.confirm({
        title: "Disable portal?",
        message: "This disables the portal record for future sign-ins.",
        confirmLabel: "Disable portal",
        targetLabel: "Portal",
        targetName: target,
        expectedValue: target,
      });
      if (!confirmed) {
        portalMutation.cancel();
        saving = false;
        return;
      }
    }

    error = null;
    saved = null;
    try {
      const outcome = await portalMutation.send({
        isStillValid: () => !scope.disposed && (targetPortalId ?? "portal-new") === intent.scope.routeKey &&

          (!editingExisting || expectedVersion === intent.input.expectedVersion),
        dispatch: async ({ input }) => await trellis.portalsPut(input).orThrow(),
      });
      if (!outcome) return;
      if (scope.disposed) {
        uncertain = true;
        error = { message: "Authorization changed during the save. Inspect the portal and reload before another change." };
        return;
      }
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Portal save outcome unknown. Inspect the portal and reload before submitting another change." };
        } else if (outcome.kind === "conflict") {
          // Another writer changed the portal after this draft's baseline.
          // Keep the draft, show what the server now holds, and let the
          // operator decide: retry against the fresh version, or reload.
          await recordConflict(target);
        } else error = failureFrom(outcome.error);
        return;
      }
      const response = outcome.value;
      // A successful own write is authoritative. Hydrate from the returned
      // record, not the request: the request's `providers: null` is a preserve
      // sentinel, and adopting it as the draft would turn the configured list
      // into an empty one the next time the operator adds a provider.
      const accepted = adoptExactLoad(draftSourceOf(response.portal));
      const recorded = recordOwnWrite(accepted.baseline.version, accepted.fields);
      portal = response.portal;
      draftBaseline = recorded.baseline;
      applyDraftFields(accepted.fields);
      conflicted = recorded.conflicted;
      newerRemote = false;
      saved = "Portal saved.";
      if (!editingExisting) await goto(resolve("/admin/portals"));
    } catch (cause) {
      error = failureFrom(cause);
    } finally {
      saving = false;
    }
  }

  /** Built-in portals save settings through the settings RPC with its version. */
  async function saveBuiltInSettings(): Promise<void> {
    if (busy || expectedVersion === null) return;
    if (conflictBlocked) {
      error = {
        message:
          "These settings conflict with a newer server version. Use Discard edits and reload current portal before saving again, or your changes will not be applied.",
      };
      return;
    }

    const key = ulid();
    const intent = captureIntent<apis.auth.PortalsLoginSettingsUpdateInput>({
      operation: "portalsLoginSettingsUpdate", targetId: portalId, label: portal?.displayName ?? portalId,
      idempotencyKey: key,
      input: {
        portalId, expectedVersion, idempotencyKey: key,
        settings: { localLogin, localRegistration, federatedRegistration, providers: providersForRequest() },
      },
      scope: { routeKey: targetPortalId ?? "portal-new" },
    });
    if (!settingsMutation.begin(intent)) return;
    saving = true;
    error = null;
    saved = null;
    // Captured before the await: an untouched providers field falls back to the
    // list this form had hydrated, never to the request's null sentinel.
    const preWrite = currentDraftFields();
    try {
      const outcome = await settingsMutation.send({
        isStillValid: () => !scope.disposed && (targetPortalId ?? "portal-new") === intent.scope.routeKey &&

          portalId === intent.targetId && expectedVersion === intent.input.expectedVersion,
        dispatch: async ({ input }) => await trellis.portalsLoginSettingsUpdate(input).orThrow(),
      });
      if (!outcome) return;
      if (scope.disposed) {
        uncertain = true;
        error = { message: "Authorization changed during the settings update. Inspect the portal and reload before another change." };
        return;
      }
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Settings update outcome unknown. Inspect the portal and reload before submitting another change." };
        } else if (outcome.kind === "conflict") {
          await recordConflict(portalId);
        } else error = failureFrom(outcome.error);
        return;
      }
      const response = outcome.value;
      // The returned settings are authoritative for the fields they carry. The
      // response's provider list is the concrete server list; only fall back to
      // the captured hydrated list when the response omits that field. The
      // request's null preserve sentinel is never treated as an empty list.
      const recorded: PortalDraftFields = {
        displayName: preWrite.displayName,
        entryUrl: preWrite.entryUrl,
        disabled: preWrite.disabled,
        localLogin: response.settings.localLogin,
        localRegistration: response.settings.localRegistration,
        federatedRegistration: response.settings.federatedRegistration,
        providers: response.settings.providers ?? preWrite.providers,
        providersEdited: false,
      };
      const ownWrite = recordOwnWrite(response.version, recorded);
      draftBaseline = ownWrite.baseline;
      applyDraftFields(recorded);
      conflicted = ownWrite.conflicted;
      if (portal) {
        portal = {
          ...portal,
          version: response.version,
          loginSettings: {
            localLogin: recorded.localLogin,
            localRegistration: recorded.localRegistration,
            federatedRegistration: recorded.federatedRegistration,
            providers: recorded.providers,
          },
        };
      }
      newerRemote = false;
      saved = "Portal settings saved.";
      const refreshed = await trellis.portalsGet({ portalId }).take();
      if (scope.disposed) return;
      if (isErr(refreshed)) {
        error = failureFrom(refreshed);
        saved = "Settings saved; refresh failed.";
        return;
      }
      // The follow-up read is a background read: it may advance the baseline
      // only while the draft is clean.
      adoptRemote(refreshed.portal, refreshed.routes, /* background */ true);
      saved = "Portal settings saved.";
    } catch (cause) {
      error = failureFrom(cause);
      if (saved) saved = "Settings saved; refresh failed.";
    } finally {
      saving = false;
    }
  }

  function resetRouteForm(): void {
    editingRouteId = null;
    routeExpectedVersion = null;
    routeParticipantId = "";
    routeDeploymentId = "";
    routeOrigin = "";
    routePriority = "0";
  }

  function editRoute(route: Route): void {
    editingRouteId = route.routeId;
    routeExpectedVersion = route.version;
    routeParticipantId = route.participantId ?? "";
    routeDeploymentId = route.deploymentId ?? "";
    routeOrigin = route.origin ?? "";
    routePriority = route.priority.toString();
  }

  function parsePriority(value: string): bigint | null {
    const trimmed = value.trim();
    if (!/^-?[0-9]+$/.test(trimmed)) return null;
    return BigInt(trimmed);
  }

  function normalizeOrigin(value: string): string | null | undefined {
    const trimmed = value.trim();
    if (trimmed === "") return null;
    let parsed: URL;
    try {
      parsed = new URL(trimmed);
    } catch {
      return undefined;
    }
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return undefined;
    if (
      parsed.pathname !== "/" || parsed.search !== "" || parsed.hash !== ""
    ) {
      return undefined;
    }
    return parsed.origin;
  }

  async function saveRoute(): Promise<void> {
    if (busy || portal === null) return;
    const priority = parsePriority(routePriority);
    if (priority === null) {
      error = { message: "Priority must be a signed decimal integer." };
      return;
    }
    const origin = normalizeOrigin(routeOrigin);
    if (origin === undefined) {
      error = { message: "Origin must be an absolute http(s) origin without a path." };
      return;
    }
    const participantId = routeParticipantId.trim() === ""
      ? null
      : routeParticipantId.trim();
    const deploymentId = routeDeploymentId.trim() === ""
      ? null
      : routeDeploymentId.trim();
    const key = ulid();
    const intent = captureIntent<apis.auth.PortalsRoutesPutInput>({
      operation: "portalsRoutesPut", targetId: portal.portalId, label: editingRouteId ?? portal.portalId,
      idempotencyKey: key,
      input: {
        portalId: portal.portalId, participantId, deploymentId, origin, priority,
        routeId: editingRouteId, expectedVersion: routeExpectedVersion, idempotencyKey: key,
      },
      scope: { routeKey: targetPortalId ?? "portal-new" },
    });
    if (!routeMutation.begin(intent)) return;
    routeBusy = true;
    if (
      participantId === null && deploymentId === null && origin === null
    ) {
      const confirmed = await confirmationModal?.confirm({
        title: "Save catch-all route?",
        message:
          "This route has no participant, deployment, or origin selector, so it matches every request to this portal.",
        confirmLabel: "Save catch-all route",
        targetLabel: "Portal",
        targetName: portal.portalId,
        expectedValue: portal.portalId,
      });
      if (!confirmed) {
        routeMutation.cancel();
        routeBusy = false;
        return;
      }
    }

    error = null;
    saved = null;
    try {
      // One in-place put: the route ID and version are retained when only the
      // selectors change, so no delete-after-put is needed.
      const outcome = await routeMutation.send({
        isStillValid: () => !scope.disposed && (targetPortalId ?? "portal-new") === intent.scope.routeKey && portal?.portalId === intent.targetId &&

          editingRouteId === intent.input.routeId && routeExpectedVersion === intent.input.expectedVersion,
        dispatch: async ({ input }) => await trellis.portalsRoutesPut(input).orThrow(),
      });
      if (!outcome) return;
      if (scope.disposed) {
        uncertain = true;
        error = { message: "Authorization changed during the route save. Inspect the portal and reload before another change." };
        return;
      }
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Route save outcome unknown. Inspect the portal and reload before submitting another change." };
        } else error = failureFrom(outcome.error);
        return;
      }
      const response = outcome.value;
      routes = [...routes.filter((route) => route.routeId !== response.route.routeId), response.route];
      saved = "Portal route saved.";
      resetRouteForm();
      const refreshed = await trellis.portalsGet({ portalId: portal.portalId }).take();
      if (scope.disposed) return;
      if (isErr(refreshed)) {
        error = failureFrom(refreshed);
        saved = "Route saved; refresh failed.";
        return;
      }
      // A route save updates only the route collection and the remote view.
      // It must not advance the portal draft's CAS baseline, or an unreviewed
      // portal edit would silently pair with a version it never saw.
      adoptRemote(refreshed.portal, refreshed.routes, /* background */ true);
    } catch (cause) {
      error = failureFrom(cause);
      if (saved) saved = "Route saved; refresh failed.";
    } finally {
      routeBusy = false;
    }
  }

  async function removeRoute(intent: MutationIntent<apis.auth.PortalsRoutesRemoveInput>): Promise<void> {
    error = null;
    saved = null;
    try {
      const outcome = await removal.send({
        isStillValid: () => !scope.disposed && (targetPortalId ?? "portal-new") === intent.scope.routeKey && portal?.portalId === intent.targetId &&

          routes.some((route) => route.routeId === intent.input.routeId && route.version === intent.input.expectedVersion),
        dispatch: async ({ input }) => await trellis.portalsRoutesRemove(input).orThrow(),
      });
      if (!outcome) return;
      if (scope.disposed) {
        uncertain = true;
        error = { message: "Authorization changed during route removal. Inspect the portal and reload before another change." };
        return;
      }
      if (outcome.kind !== "succeeded") {
        if (outcome.kind === "unknown") {
          uncertain = true;
          error = { message: "Route removal outcome unknown. Inspect the portal and reload before submitting another change." };
        } else error = failureFrom(outcome.error);
        return;
      }
      routes = routes.filter((item) => item.routeId !== intent.input.routeId);
      saved = "Portal route removed.";
      if (editingRouteId === intent.input.routeId) resetRouteForm();
      const refreshed = await trellis.portalsGet({ portalId: intent.targetId }).take();
      if (scope.disposed) return;
      if (isErr(refreshed)) {
        error = failureFrom(refreshed);
        saved = "Route removed; refresh failed.";
        return;
      }
      adoptRemote(refreshed.portal, refreshed.routes, /* background */ true);
    } catch (cause) {
      error = failureFrom(cause);
      if (saved) saved = "Route removed; refresh failed.";
    } finally {
      routeBusy = false;
    }
  }

  async function requestRemoveRoute(route: Route): Promise<void> {
    if (busy) return;
    const key = ulid();
    const intent = captureIntent<apis.auth.PortalsRoutesRemoveInput>({
      operation: "portalsRoutesRemove", targetId: route.portalId, label: route.routeId,
      expectedValue: route.routeId, idempotencyKey: key,
      input: { routeId: route.routeId, expectedVersion: route.version, idempotencyKey: key },
      scope: { routeKey: targetPortalId ?? "portal-new" },
    });
    if (!removal.begin(intent)) return;
    routeBusy = true;
    const confirmed = await confirmationModal?.confirm({
      title: "Remove portal route?",
      message: "This removes the route rule from the portal.",
      confirmLabel: "Remove route",
      targetLabel: "Route",
      targetName: `${route.participantId ?? "any participant"} / ${route.deploymentId ?? "any deployment"}`,
      expectedValue: route.routeId,
      details: `Route ID: ${route.routeId}`,
    });
    if (confirmed) await removeRoute(intent);
    else {
      removal.cancel();
      routeBusy = false;
    }
  }

  onMount(() => {
    void load();
    return () => scope.dispose();
  });
</script>

<section class="mx-auto max-w-5xl space-y-4">
  <div>
    <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Back to portals</a>
  </div>

  {#if error}
    <Notice variant="error">
      {error.message}
      {#if error.id}<span class="ml-1 text-xs opacity-70">Reference: {error.id}</span>{/if}
    </Notice>
  {/if}
  {#if conflictBlocked}
    <Notice variant="warning" role="status">
      This draft conflicts with a newer server version. Saving is blocked until you
      discard your edits and reload the current portal; your unsaved edits are kept
      until then so you can reapply the changes you intended.
      <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={reloadPortal}>
        Discard edits and reload current portal
      </button>
    </Notice>
  {:else if newerRemote && draftDirty}
    <Notice variant="warning" role="status">
      Newer portal data exists on the server. Your unsaved edits are kept against
      their original version; saving now reports a conflict rather than
      overwriting the other change.
      <button class="btn btn-ghost btn-xs ml-2" type="button" onclick={reloadPortal}>Discard edits and reload</button>
    </Notice>
  {/if}
  {#if saved}
    <Notice variant="success">{saved}</Notice>
  {/if}

  {#if loading}
    <Panel><LoadingState label="Loading portal" /></Panel>
  {:else if editingExisting && notFound}
    <EmptyState
      title="Portal unavailable"
      description={`No portal matches '${targetPortalId ?? ""}'. It may have been removed, access may be denied, or the link may be stale.`}
      class="m-5"
    >
      {#snippet actions()}
        <a class="btn btn-outline btn-sm" href={resolve("/admin/portals")}>Back to portals</a>
      {/snippet}
    </EmptyState>
  {:else}
    <form class="space-y-4" onsubmit={save}>
      <Panel title="Portal" eyebrow={builtIn ? "Built-in" : undefined}>
        {#snippet actions()}
          <span class="trellis-metadata text-[0.65rem]">
            {portal ? `Version ${portal.version}` : "New portal"}
          </span>
        {/snippet}

        <div class="grid gap-3 md:grid-cols-[5.5rem_minmax(0,1fr)_minmax(0,1fr)]">
          <label class="form-control">
            <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Enabled</span>
            <span class="flex h-8 items-center px-1">
              <input
                class="toggle toggle-sm toggle-primary"
                type="checkbox"
                checked={!disabled}
                disabled={busy || builtIn}
                onchange={(event) => { disabled = !event.currentTarget.checked; }}
              />
            </span>
          </label>
          <label class="form-control">
            <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Portal ID</span>
            <input
              class="input input-bordered input-sm font-mono"
              bind:value={portalId}
              disabled={busy || editingExisting}
              required={!editingExisting}
            />
          </label>
          <label class="form-control">
            <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Display name</span>
            <input
              class="input input-bordered input-sm"
              bind:value={displayName}
              disabled={busy || builtIn}
              required={!builtIn}
            />
          </label>
          <label class="form-control md:col-span-3">
            <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Entry URL</span>
            <input
              class="input input-bordered input-sm font-mono"
              bind:value={entryUrl}
              disabled={busy || builtIn}
            />
          </label>
        </div>
      </Panel>

      <Panel title="Login settings" eyebrow="Real portal settings">
        <div class="space-y-3">
          <label class="flex items-start justify-between gap-3 rounded border border-base-300 px-3 py-2 text-sm">
            <span class="min-w-0">
              <span class="block font-medium">Local login</span>
              <span class="mt-0.5 block text-xs text-base-content/60">Username and password sign-in.</span>
            </span>
            <input class="toggle toggle-sm toggle-primary" type="checkbox" bind:checked={localLogin} disabled={busy} />
          </label>

          <label class="flex items-start justify-between gap-3 rounded border border-base-300 px-3 py-2 text-sm">
            <span class="min-w-0">
              <span class="block font-medium">Local registration</span>
              <span class="mt-0.5 block text-xs text-base-content/60">Username and password signup.</span>
            </span>
            <input class="toggle toggle-sm toggle-primary" type="checkbox" bind:checked={localRegistration} disabled={busy} />
          </label>

          <label class="flex items-start justify-between gap-3 rounded border border-base-300 px-3 py-2 text-sm">
            <span class="min-w-0">
              <span class="block font-medium">Federated registration</span>
              <span class="mt-0.5 block text-xs text-base-content/60">Signup through configured providers.</span>
            </span>
            <input class="toggle toggle-sm toggle-primary" type="checkbox" bind:checked={federatedRegistration} disabled={busy} />
          </label>

          <div class="rounded border border-base-300 px-3 py-2 text-sm">
            <div class="flex flex-wrap items-baseline justify-between gap-2">
              <span class="font-medium">Configured provider IDs</span>
              <span class="trellis-metadata text-[0.65rem]">
                {providersEdited ? "explicit list" : "preserve configured IDs"}
              </span>
            </div>
            <p class="mt-1 text-xs text-base-content/60">
              This is the configured provider ID list, not a provider catalog. On update,
              leaving it untouched preserves the server's configured IDs; it does not mean
              "all providers". An explicitly empty list disables federated sign-in.
            </p>
            <div class="mt-2 flex flex-wrap gap-1">
              {#each providers ?? [] as providerId (providerId)}
                <span class="badge badge-outline badge-sm gap-1">
                  <span class="trellis-identifier">{providerId}</span>
                  <button
                    type="button"
                    class="text-error"
                    aria-label={`Remove provider ${providerId}`}
                    disabled={busy}
                    onclick={() => removeProviderId(providerId)}
                  >×</button>
                </span>
              {:else}
                <span class="text-xs text-base-content/60">
                  {providersEdited ? "No provider IDs configured." : "Provider list unchanged."}
                </span>
              {/each}
            </div>
            <div class="mt-2 flex gap-2">
              <input
                class="input input-bordered input-sm font-mono grow"
                bind:value={providerDraft}
                placeholder="provider-id"
                aria-label="Provider ID"
                disabled={busy}
              />
              <button class="btn btn-outline btn-xs" type="button" disabled={busy || providerDraft.trim() === ""} onclick={addProviderId}>
                Add ID
              </button>
              <button class="btn btn-ghost btn-xs" type="button" disabled={busy} onclick={clearProviders}>
                Send empty list
              </button>
            </div>
          </div>
        </div>
      </Panel>

      {#if editingExisting && portal}
        <Panel title="Portal routes" eyebrow={`${routes.length} route${routes.length === 1 ? "" : "s"}`}>
          <div class="grid gap-3 lg:grid-cols-[minmax(0,1fr)_20rem]">
            <DataTable class="border-b border-base-300 bg-base-100/30">
              <thead>
                <tr>
                  <th>Participant</th>
                  <th>Deployment</th>
                  <th>Origin</th>
                  <th>Priority</th>
                  <th class="text-right">Actions</th>
                </tr>
              </thead>
              <tbody>
                {#each sortedRoutes as route (route.routeId)}
                  <tr>
                    <td class="trellis-identifier text-xs">{route.participantId ?? "any"}</td>
                    <td class="trellis-identifier text-xs">{route.deploymentId ?? "any"}</td>
                    <td class="trellis-identifier text-xs">{route.origin ?? "any"}</td>
                    <td class="text-xs">{route.priority}</td>
                    <td class="text-right">
                      <button class="btn btn-ghost btn-xs" type="button" onclick={() => editRoute(route)} disabled={busy}>Edit</button>
                      <button class="btn btn-error btn-outline btn-xs" type="button" onclick={() => requestRemoveRoute(route)} disabled={busy}>Remove</button>
                    </td>
                  </tr>
                {:else}
                  <tr><td class="text-xs text-base-content/60" colspan="5">No routes target this portal.</td></tr>
                {/each}
              </tbody>
            </DataTable>

            <div class="space-y-2 rounded border border-base-300 p-3">
              <div class="text-xs uppercase tracking-wide text-base-content/55">
                {editingRouteId ? "Edit route" : "Add route"}
              </div>
              <label class="form-control">
                <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Participant ID</span>
                <input class="input input-bordered input-sm font-mono" bind:value={routeParticipantId} disabled={busy} placeholder="Blank for any participant" />
              </label>
              <label class="form-control">
                <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Deployment ID</span>
                <input class="input input-bordered input-sm font-mono" bind:value={routeDeploymentId} disabled={busy} placeholder="Blank for any deployment" />
              </label>
              <label class="form-control">
                <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Origin</span>
                <input class="input input-bordered input-sm font-mono" bind:value={routeOrigin} disabled={busy} placeholder="https://example.com" />
              </label>
              <label class="form-control">
                <span class="label-text text-xs uppercase tracking-wide text-base-content/55">Priority</span>
                <input class="input input-bordered input-sm font-mono" bind:value={routePriority} disabled={busy} />
              </label>
              <div class="flex justify-end gap-2">
                <button class="btn btn-ghost btn-xs" type="button" onclick={resetRouteForm} disabled={busy}>Clear</button>
                <button class="btn btn-outline btn-xs" type="button" onclick={saveRoute} disabled={busy}>
                  {routeBusy ? "Saving" : editingRouteId ? "Update route" : "Save route"}
                </button>
              </div>
            </div>
          </div>
        </Panel>
      {/if}

      <div class="flex justify-end gap-2">
        <a class="btn btn-ghost btn-sm" href={resolve("/admin/portals")}>Cancel</a>
        {#if builtIn}
          <button class="btn btn-outline btn-sm" type="button" disabled={busy || conflictBlocked} onclick={saveBuiltInSettings}>
            {saving ? "Saving" : "Save login settings"}
          </button>
        {:else}
          <button class="btn btn-outline btn-sm" type="submit" disabled={busy || conflictBlocked}>
            {saving ? "Saving" : "Save portal"}
          </button>
        {/if}
      </div>
    </form>
  {/if}
</section>

<ConfirmationModal bind:this={confirmationModal} />
