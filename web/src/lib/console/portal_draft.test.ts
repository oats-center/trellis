import { deepEqual, equal } from "node:assert/strict";

import {
  adoptBackgroundRead,
  adoptExactLoad,
  adoptOwnWrite,
  discardDraft,
  draftIsDirty,
  type PortalConflictState,
  type PortalDraftBaseline,
  type PortalDraftFields,
  type PortalDraftSource,
  recordConflict,
  recordOwnWrite,
  recordReload,
} from "./portal_draft.ts";

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};

function portal(
  version: bigint,
  overrides: Partial<PortalDraftSource> = {},
): PortalDraftSource {
  return {
    version,
    displayName: "Portal",
    entryUrl: "https://portal.example",
    disabled: false,
    localLogin: true,
    localRegistration: true,
    federatedRegistration: false,
    providers: ["oidc"],
    ...overrides,
  };
}

Deno.test("V14 an exact load adopts the record and its version together", () => {
  const loaded = adoptExactLoad(portal(1n));
  equal(loaded.baseline.version, 1n);
  equal(loaded.fields.displayName, "Portal");
  equal(loaded.fields.providersEdited, false);
});

Deno.test("V14 a dirty draft keeps its baseline through a background read", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = {
    ...fields,
    displayName: "Edited locally",
  };
  equal(draftIsDirty(baseline, edited), true);

  const after = adoptBackgroundRead(
    baseline,
    edited,
    portal(2n, {
      displayName: "Changed by another operator",
      localLogin: false,
    }),
  );

  equal(after.adopted, false, "a dirty draft must not adopt a background read");
  equal(
    after.baseline?.version,
    1n,
    "the write baseline stays the version the draft was authored against",
  );
  equal(after.fields.displayName, "Edited locally");
  equal(
    after.newerRemote,
    true,
    "the page is told newer remote data exists",
  );
});

Deno.test("V14 a clean draft adopts the complete remote record and version", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const after = adoptBackgroundRead(
    baseline,
    fields,
    portal(2n, {
      displayName: "Remote name",
    }),
  );
  equal(after.adopted, true);
  equal(after.baseline?.version, 2n);
  equal(after.fields.displayName, "Remote name");
});

Deno.test("V15 an own write makes its version and fields the new baseline", () => {
  const { fields } = adoptExactLoad(portal(1n));
  const submitted: PortalDraftFields = {
    ...fields,
    displayName: "Saved name",
    providers: ["saml"],
    providersEdited: true,
  };
  const baseline = adoptOwnWrite(3n, submitted);
  equal(baseline.version, 3n);
  equal(baseline.fields.displayName, "Saved name");
  deepEqual(baseline.fields.providers, ["saml"]);
  equal(
    baseline.fields.providersEdited,
    false,
    "a submitted provider list is now the recorded baseline",
  );
  equal(
    draftIsDirty(baseline, baseline.fields),
    false,
    "the just-saved draft is clean",
  );
});

Deno.test("V15 an own write is not rebased by a stale background read", () => {
  const { fields } = adoptExactLoad(portal(1n));
  const baseline = adoptOwnWrite(3n, { ...fields, displayName: "Mine" });
  // Another writer commits v4 before this page's follow-up read lands.
  const after = adoptBackgroundRead(
    baseline,
    baseline.fields,
    portal(4n, { displayName: "Theirs" }),
  );
  equal(
    after.adopted,
    true,
    "a clean draft may adopt a later background read",
  );
  equal(after.baseline?.version, 4n);
});

Deno.test("V14 a newer remote version cannot pair with an old draft", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = {
    ...fields,
    entryUrl: "https://mine.example",
  };
  const after = adoptBackgroundRead(baseline, edited, portal(2n));
  // The exact defect: version 2 must never accompany values authored at v1.
  equal(after.baseline?.version === 2n, false);
  equal(
    draftIsDirty(after.baseline, after.fields),
    true,
    "the draft is still pending against its original baseline",
  );
});

Deno.test("V15 an explicit discard replaces draft and baseline atomically", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = { ...fields, displayName: "Unsaved" };
  equal(draftIsDirty(baseline, edited), true);

  const discarded = discardDraft(portal(2n, { displayName: "Remote" }));
  equal(discarded.baseline.version, 2n);
  equal(discarded.fields.displayName, "Remote");
  equal(draftIsDirty(discarded.baseline, discarded.fields), false);
});

Deno.test("V15 provider edit intent alone counts as a dirty draft", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = { ...fields, providersEdited: true };
  equal(draftIsDirty(baseline, edited), true);
  const after = adoptBackgroundRead(baseline, edited, portal(2n));
  equal(after.baseline?.version, 1n);
});

Deno.test("V15 a null baseline has no dirty draft to protect", () => {
  equal(
    draftIsDirty(null, {
      displayName: "New portal",
      entryUrl: "",
      disabled: false,
      localLogin: true,
      localRegistration: true,
      federatedRegistration: false,
      providers: null,
      providersEdited: false,
    }),
    false,
  );
});

Deno.test("V14 a repeated background read with an unchanged draft is stable", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = { ...fields, displayName: "Mine" };
  const first = adoptBackgroundRead(baseline, edited, portal(2n));
  const second = adoptBackgroundRead(first.baseline, first.fields, portal(3n));
  equal(second.baseline?.version, 1n);
  equal(second.fields.displayName, "Mine");
});

Deno.test("Z04 a rejected stale write records a conflict without adopting the newer version", () => {
  const { baseline, fields } = adoptExactLoad(portal(1n));
  const edited: PortalDraftFields = { ...fields, displayName: "Mine at v1" };
  const state: PortalConflictState = recordConflict(baseline);
  equal(state.conflicted, true);
  equal(
    state.baseline?.version,
    1n,
    "a rejected write must never pair old values with another writer's version",
  );
  equal(
    draftIsDirty(state.baseline, edited),
    true,
    "the retained draft is still pending against its original baseline",
  );
});

Deno.test("Z04 an explicit reload adopts only a successful exact read", () => {
  const { baseline } = adoptExactLoad(portal(1n));
  const conflicted = recordConflict(baseline);

  const afterFailure = recordReload(conflicted, { ok: false });
  equal(
    afterFailure.conflicted,
    true,
    "a failed reload keeps the conflict so Save stays blocked",
  );
  equal(afterFailure.baseline?.version, 1n);

  const afterSuccess = recordReload(conflicted, {
    ok: true,
    portal: portal(2n, { displayName: "Theirs" }),
  });
  equal(afterSuccess.conflicted, false);
  equal(afterSuccess.baseline?.version, 2n);
  equal(afterSuccess.baseline?.fields.displayName, "Theirs");
  equal(
    draftIsDirty(afterSuccess.baseline, afterSuccess.baseline!.fields),
    false,
  );
});

Deno.test("Z04 a successful own write keeps its version even when the follow-up read fails", () => {
  const { fields } = adoptExactLoad(portal(1n));
  const accepted: PortalDraftFields = { ...fields, displayName: "Saved" };
  const state = recordOwnWrite(3n, accepted);
  equal(state.conflicted, false);
  equal(
    state.baseline?.version,
    3n,
    "the known successful version is retained for a later save",
  );
  equal(state.baseline?.fields.displayName, "Saved");
});

Deno.test("V14 fieldsFromPortal clears provider edit intent", () => {
  const loaded = adoptExactLoad(portal(1n, { providers: ["a", "b"] }));
  deepEqual(loaded.fields.providers, ["a", "b"]);
  equal(loaded.fields.providersEdited, false);
  const baseline: PortalDraftBaseline = loaded.baseline;
  equal(baseline.version, 1n);
});

Deno.test("Z02 a successful write hydrates the server's provider list, not a null sentinel", () => {
  // A metadata-only update sends `providers: null` to mean preserve. The
  // response record carries the configured list, and hydrating from it must
  // keep those IDs so a later Add ID appends rather than replaces.
  const returned = portal(2n, { providers: ["provider-a"] });
  const accepted = adoptExactLoad(returned);
  deepEqual(
    accepted.fields.providers,
    ["provider-a"],
    "the configured provider list survives response hydration",
  );
  equal(accepted.fields.providers === null, false);
  equal(accepted.fields.providersEdited, false);
  equal(accepted.baseline.version, 2n);

  // Appending to that hydrated list is an explicit edit that keeps provider-a.
  const appended: PortalDraftFields = {
    ...accepted.fields,
    providers: [...(accepted.fields.providers ?? []), "provider-b"],
    providersEdited: true,
  };
  deepEqual(appended.providers, ["provider-a", "provider-b"]);
  equal(draftIsDirty(accepted.baseline, appended), true);
});

Deno.test("Z02 two consecutive successful saves keep the server provider list", () => {
  const first = adoptExactLoad(portal(1n, { providers: ["provider-a"] }));
  // First save is metadata-only: the request preserves providers, and the
  // response still reports provider-a. Hydration must not erase it.
  const afterFirst = adoptExactLoad(portal(2n, {
    displayName: "Renamed",
    providers: ["provider-a"],
  }));
  deepEqual(afterFirst.fields.providers, ["provider-a"]);
  equal(afterFirst.fields.displayName, "Renamed");
  equal(afterFirst.baseline.version, 2n);

  // Second save is explicit: the operator added provider-b, so both persist.
  const afterSecond = adoptExactLoad(portal(3n, {
    displayName: "Renamed",
    providers: ["provider-a", "provider-b"],
  }));
  deepEqual(afterSecond.fields.providers, ["provider-a", "provider-b"]);
  equal(draftIsDirty(afterSecond.baseline, afterSecond.fields), false);
});

Deno.test("Z02 an explicit empty provider list is replacement, not preservation", () => {
  const loaded = adoptExactLoad(portal(1n, { providers: ["provider-a"] }));
  const cleared: PortalDraftFields = {
    ...loaded.fields,
    providers: [],
    providersEdited: true,
  };
  equal(draftIsDirty(loaded.baseline, cleared), true);
  const accepted = adoptExactLoad(portal(2n, { providers: [] }));
  deepEqual(accepted.fields.providers, []);
  equal(accepted.fields.providers === null, false);
});
