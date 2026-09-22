/**
 * Portal draft adoption rules.
 *
 * A portal form has three distinct records, and conflating them is how an
 * operator's unreviewed edit gets paired with another writer's version:
 *
 * - `baseline`: the portal record **and version** the editable values were
 *   authored against. A portal/settings write sends this version.
 * - `draft`: the editable metadata/login fields, including explicit
 *   provider-list edit intent.
 * - `remote`: the most recently read server state, used for status and route
 *   presentation.
 *
 * A successful own write is authoritative for both fields and version. A
 * background read is **not**: it may only advance the baseline when the draft
 * is clean, because only then is there no unreviewed edit to protect.
 */

/** Editable portal fields, independent of which version last supplied them. */
export type PortalDraftFields = {
  readonly displayName: string;
  readonly entryUrl: string;
  readonly disabled: boolean;
  readonly localLogin: boolean;
  readonly localRegistration: boolean;
  readonly federatedRegistration: boolean;
  /** `null` preserves the server's configured provider IDs on update. */
  readonly providers: string[] | null;
  /** True once the operator explicitly edited the provider list. */
  readonly providersEdited: boolean;
};

/** The record and version an editable draft was authored against. */
export type PortalDraftBaseline = {
  readonly version: bigint;
  readonly fields: PortalDraftFields;
};

/** The subset of a portal record the draft rules read. */
export type PortalDraftSource = {
  readonly version: bigint;
  readonly displayName: string;
  readonly entryUrl: string | null;
  readonly disabled: boolean;
  readonly localLogin: boolean;
  readonly localRegistration: boolean;
  readonly federatedRegistration: boolean;
  readonly providers: readonly string[];
};

/** Fields from a loaded or written portal record, with edit intent cleared. */
export function fieldsFromPortal(portal: PortalDraftSource): PortalDraftFields {
  return {
    displayName: portal.displayName,
    entryUrl: portal.entryUrl ?? "",
    disabled: portal.disabled,
    localLogin: portal.localLogin,
    localRegistration: portal.localRegistration,
    federatedRegistration: portal.federatedRegistration,
    providers: [...portal.providers],
    providersEdited: false,
  };
}

/** True when the editable fields differ from the baseline they were authored against. */
export function draftIsDirty(
  baseline: PortalDraftBaseline | null,
  fields: PortalDraftFields,
): boolean {
  if (baseline === null) return false;
  const base = baseline.fields;
  if (fields.providersEdited) return true;
  return fields.displayName !== base.displayName ||
    fields.entryUrl !== base.entryUrl ||
    fields.disabled !== base.disabled ||
    fields.localLogin !== base.localLogin ||
    fields.localRegistration !== base.localRegistration ||
    fields.federatedRegistration !== base.federatedRegistration ||
    !sameProviders(fields.providers, base.providers);
}

function sameProviders(
  left: readonly string[] | null,
  right: readonly string[] | null,
): boolean {
  if (left === null || right === null) return left === right;
  return left.length === right.length &&
    left.every((value, index) => value === right[index]);
}

/** First successful exact load: baseline, draft, and remote all come from it. */
export function adoptExactLoad(portal: PortalDraftSource): {
  readonly baseline: PortalDraftBaseline;
  readonly fields: PortalDraftFields;
} {
  const fields = fieldsFromPortal(portal);
  return { baseline: { version: portal.version, fields }, fields };
}

/**
 * Successful own write: the response's version and the values actually
 * submitted become the new baseline, so a second save uses the new version
 * without rebasing onto anyone else's change.
 */
export function adoptOwnWrite(
  version: bigint,
  submitted: PortalDraftFields,
): PortalDraftBaseline {
  return { version, fields: { ...submitted, providersEdited: false } };
}

/**
 * A background read.
 *
 * While the draft is dirty, only the remote view advances: the draft keeps its
 * original version so the operator's next save reports the conflict instead of
 * silently overwriting a change they never reviewed. While the draft is clean,
 * the complete remote record and its version are adopted together.
 */
export function adoptBackgroundRead(
  baseline: PortalDraftBaseline | null,
  fields: PortalDraftFields,
  remote: PortalDraftSource,
): {
  readonly baseline: PortalDraftBaseline | null;
  readonly fields: PortalDraftFields;
  readonly adopted: boolean;
  readonly newerRemote: boolean;
} {
  if (draftIsDirty(baseline, fields)) {
    return {
      baseline,
      fields,
      adopted: false,
      newerRemote: baseline !== null && baseline.version !== remote.version,
    };
  }
  const adopted = adoptExactLoad(remote);
  return {
    baseline: adopted.baseline,
    fields: adopted.fields,
    adopted: true,
    newerRemote: false,
  };
}

/**
 * Explicit operator reload/discard. Replaces the draft and baseline atomically,
 * which is the only way a dirty draft may adopt a newer remote version.
 */
export function discardDraft(portal: PortalDraftSource): {
  readonly baseline: PortalDraftBaseline;
  readonly fields: PortalDraftFields;
} {
  return adoptExactLoad(portal);
}

/**
 * Conflict state for one editable portal draft.
 *
 * A rejected stale write is **not** an own write. Both the draft and its
 * original baseline stay exactly as they were, so no ordinary save can acquire
 * a version the draft never saw; only an explicit reload adopts a new record.
 */
export type PortalConflictState = {
  readonly baseline: PortalDraftBaseline | null;
  readonly conflicted: boolean;
};

/** Records a stale-write rejection: baseline and draft are untouched. */
export function recordConflict(
  baseline: PortalDraftBaseline | null,
): PortalConflictState {
  return { baseline, conflicted: true };
}

/**
 * Records a definite successful own write: the returned version and the
 * accepted fields become the new baseline even when a later follow-up read
 * fails, so the known successful version is never lost.
 */
export function recordOwnWrite(
  version: bigint,
  accepted: PortalDraftFields,
): PortalConflictState {
  return { baseline: adoptOwnWrite(version, accepted), conflicted: false };
}

/**
 * Records an explicit reload outcome.
 *
 * Adoption happens only after the exact read succeeds, so a failed read must
 * leave the conflicted draft and its baseline exactly as they were.
 */
export function recordReload(
  current: PortalConflictState,
  outcome: { readonly ok: true; readonly portal: PortalDraftSource } | {
    readonly ok: false;
  },
): PortalConflictState {
  if (!outcome.ok) return current;
  return {
    baseline: adoptExactLoad(outcome.portal).baseline,
    conflicted: false,
  };
}
