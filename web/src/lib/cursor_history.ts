export type CursorHistory = {
  cursor?: string;
  back: string[];
};

/** Advances one cursor page while retaining the current page for navigation back. */
export function nextCursorPage(
  history: CursorHistory,
  nextCursor: string,
): CursorHistory {
  return { cursor: nextCursor, back: [...history.back, history.cursor ?? ""] };
}

/** Returns to the previous cursor page. */
export function previousCursorPage(history: CursorHistory): CursorHistory {
  const back = history.back.slice(0, -1);
  return { cursor: history.back.at(-1) || undefined, back };
}

/** Returns cursor navigation to the first page. */
export function resetCursorHistory(): CursorHistory {
  return { back: [] };
}
