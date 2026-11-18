import { errorMessage } from "$lib/format";
import type { OptionalReadState } from "$lib/console/events_detail.ts";

/** One page of a side panel's list, as the server returned it. */
export type PanelPage<T> = { items: T[]; nextCursor?: string };

/** The page a panel asks for: a keyset position and a size. */
export type PanelPageRequest = { cursor?: string; limit: number };

/** The most rows one request may ask for. */
const MAX_PAGE_LIMIT = 500;

/**
 * A side panel's list that loads one bounded page at a time.
 *
 * A refresh asks for as many rows as the panel already shows, in one request,
 * so a list someone expanded stays expanded; a change of filter starts over at
 * one page. A newer read always wins over an older one still in flight, and a
 * failed read never shows as an empty list.
 */
export class PanelPages<T> {
  #items = $state.raw<T[]>([]);
  #nextCursor = $state<string | undefined>();
  #state = $state<OptionalReadState>("pending");
  #error = $state<string | null>(null);
  #moreError = $state<string | null>(null);
  #loadingMore = $state(false);
  #sequence = 0;
  #loadedKey: string | null = null;
  #disposed = false;
  readonly #pageSize: number;
  readonly #filterKey: () => string;
  readonly #read: (page: PanelPageRequest) => Promise<PanelPage<T>>;
  readonly #identity: (item: T) => string;

  /**
   * @param options.pageSize Rows in the first page and in each "show more".
   * @param options.filterKey Identity of the current filters; a change starts over.
   * @param options.read Reads one page with the current filters.
   * @param options.identity A row's stable identity, used to drop repeats.
   */
  constructor(options: {
    pageSize: number;
    filterKey: () => string;
    read: (page: PanelPageRequest) => Promise<PanelPage<T>>;
    identity: (item: T) => string;
  }) {
    this.#pageSize = options.pageSize;
    this.#filterKey = options.filterKey;
    this.#read = options.read;
    this.#identity = options.identity;
  }

  /** Rows loaded so far, in server order. */
  get items(): T[] {
    return this.#items;
  }

  /** Whether the server has rows after the loaded ones. */
  get hasMore(): boolean {
    return this.#nextCursor !== undefined;
  }

  /** Whether the last read succeeded, failed, or has not finished yet. */
  get state(): OptionalReadState {
    return this.#state;
  }

  /** Why the last read failed, when it did. */
  get error(): string | null {
    return this.#error;
  }

  /** Why the last "show more" failed, when it did; the loaded rows stay. */
  get moreError(): string | null {
    return this.#moreError;
  }

  /** Whether a "show more" read is in flight. */
  get loadingMore(): boolean {
    return this.#loadingMore;
  }

  /** Reads the panel again from the start. Never throws. */
  async reload(): Promise<void> {
    const key = this.#filterKey();
    const sameFilter = key === this.#loadedKey;
    const limit = Math.min(
      MAX_PAGE_LIMIT,
      Math.max(this.#pageSize, sameFilter ? this.#items.length : 0),
    );
    const sequence = ++this.#sequence;
    if (!sameFilter) this.#state = "pending";
    try {
      const page = await this.#read({ limit });
      if (!this.#owns(sequence)) return;
      this.#items = this.#unique(page.items);
      this.#nextCursor = page.nextCursor;
      this.#state = "ready";
      this.#error = null;
      this.#moreError = null;
      this.#loadedKey = key;
    } catch (cause) {
      if (!this.#owns(sequence)) return;
      this.#items = [];
      this.#nextCursor = undefined;
      this.#state = "unavailable";
      this.#error = errorMessage(cause);
      this.#loadedKey = null;
    } finally {
      if (this.#owns(sequence)) this.#loadingMore = false;
    }
  }

  /** Appends the next page. Never throws. */
  async more(): Promise<void> {
    const cursor = this.#nextCursor;
    if (cursor === undefined || this.#loadingMore) return;
    const sequence = ++this.#sequence;
    this.#loadingMore = true;
    this.#moreError = null;
    try {
      const page = await this.#read({ cursor, limit: this.#pageSize });
      if (!this.#owns(sequence)) return;
      this.#items = this.#unique([...this.#items, ...page.items]);
      this.#nextCursor = page.nextCursor;
    } catch (cause) {
      if (!this.#owns(sequence)) return;
      this.#moreError = errorMessage(cause);
    } finally {
      if (this.#owns(sequence)) this.#loadingMore = false;
    }
  }

  /** Stops every read in flight from publishing. */
  dispose(): void {
    this.#disposed = true;
    this.#sequence += 1;
  }

  #owns(sequence: number): boolean {
    return !this.#disposed && sequence === this.#sequence;
  }

  /** Live rows can move between pages; each one is shown once. */
  #unique(items: T[]): T[] {
    const seen = new Set<string>();
    return items.filter((item) => {
      const id = this.#identity(item);
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    });
  }
}
