/**
 * Unsaved form edits kept across a reread of the record they edit.
 *
 * A form holds three sets of values by field name: the draft (what is typed),
 * the base (the saved value each draft field was typed over), and the saved
 * values just read. A reread never changes what was typed. A field left alone
 * takes the saved value. A field typed over a saved value that has since
 * changed is a conflict: the form shows both values and saves nothing until
 * the operator keeps theirs or takes the saved one. With no conflict left,
 * every base equals the saved value, so saving against the newer revision
 * overwrites nothing someone else saved.
 */

/** A form's values by field name. */
export type FormValues = Record<string, unknown>;

/** A draft and the saved values each of its fields was typed over. */
export type DraftState<T extends FormValues> = { draft: T; base: T };

/** Structural equality for form values: primitives, arrays and plain objects. */
export function sameValue(left: unknown, right: unknown): boolean {
  if (Object.is(left, right)) return true;
  if (
    typeof left !== "object" ||
    typeof right !== "object" ||
    left === null ||
    right === null
  )
    return false;
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right)) return false;
    return (
      left.length === right.length &&
      left.every((item, index) => sameValue(item, right[index]))
    );
  }
  const leftRecord = left as Record<string, unknown>;
  const rightRecord = right as Record<string, unknown>;
  const keys = new Set([
    ...Object.keys(leftRecord),
    ...Object.keys(rightRecord),
  ]);
  for (const key of keys)
    if (!sameValue(leftRecord[key], rightRecord[key])) return false;
  return true;
}

/**
 * Brings a draft onto newly read saved values. Fields left alone take the
 * saved value; typed fields keep what was typed, and keep their base when the
 * saved value moved under them, which makes them conflicts.
 */
export function rebaseDraft<T extends FormValues>(
  state: DraftState<T>,
  saved: T,
): DraftState<T> {
  const draft: FormValues = { ...state.draft };
  const base: FormValues = { ...state.base };
  for (const key of Object.keys(saved)) {
    if (sameValue(state.draft[key], state.base[key])) {
      draft[key] = saved[key];
      base[key] = saved[key];
    } else if (sameValue(state.draft[key], saved[key])) base[key] = saved[key];
  }
  return { draft: draft as T, base: base as T };
}

/**
 * The fields typed over a saved value that changed since, in `saved`'s field
 * order. Saving waits until there are none.
 */
export function draftConflicts<T extends FormValues>(
  state: DraftState<T>,
  saved: T,
): (keyof T & string)[] {
  return Object.keys(saved).filter(
    (key) =>
      !sameValue(state.base[key], saved[key]) &&
      !sameValue(state.draft[key], saved[key]),
  );
}

/**
 * Settles one conflict: `keep` saves the typed value over the newer saved
 * one; `take` replaces it with the saved value.
 */
export function settleConflict<T extends FormValues>(
  state: DraftState<T>,
  saved: T,
  field: keyof T & string,
  choice: "keep" | "take",
): DraftState<T> {
  return {
    draft:
      choice === "take"
        ? { ...state.draft, [field]: saved[field] }
        : state.draft,
    base: { ...state.base, [field]: saved[field] },
  };
}
