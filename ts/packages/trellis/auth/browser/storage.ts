import { publicKeyBase64urlFromSeed } from "../keys.ts";

const DB_NAME = "trellis-auth";
const DB_VERSION = 3;
const STORE_NAME = "installations";

type BrowserInstallationRecord = {
  id: string;
  generation: number;
  seed?: Uint8Array;
  sessionKey?: string;
  loginSessionId?: string;
  expiresAt?: number | null;
  pendingFlowId?: string;
};

/** Browser-owned installation credential and optional remembered login. */
export type BrowserSessionCredential = Readonly<{
  generation: number;
  seed: Uint8Array;
  sessionKey: string;
  loginSessionId?: string;
  expiresAt?: number | null;
  pendingFlowId?: string;
}>;

const temporaryInstallations = new Map<string, BrowserInstallationRecord>();

function openDB(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DB_NAME, DB_VERSION);
    request.onerror = () => reject(request.error);
    request.onsuccess = () => resolve(request.result);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE_NAME)) {
        db.createObjectStore(STORE_NAME, { keyPath: "id" });
      }
    };
  });
}

function validGeneration(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function credential(
  record: BrowserInstallationRecord | undefined,
): BrowserSessionCredential | undefined {
  if (
    !record || !validGeneration(record.generation) ||
    !(record.seed instanceof Uint8Array) || !record.sessionKey
  ) return undefined;
  return {
    generation: record.generation,
    seed: record.seed.slice(),
    sessionKey: record.sessionKey,
    ...(record.loginSessionId === undefined
      ? {}
      : { loginSessionId: record.loginSessionId }),
    ...(record.expiresAt === undefined ? {} : { expiresAt: record.expiresAt }),
    ...(record.pendingFlowId === undefined
      ? {}
      : { pendingFlowId: record.pendingFlowId }),
  };
}

function expired(record: BrowserInstallationRecord, now: number): boolean {
  return record.loginSessionId !== undefined && record.expiresAt !== null &&
    record.expiresAt !== undefined && record.expiresAt <= now;
}

function tombstone(
  record: BrowserInstallationRecord,
): BrowserInstallationRecord {
  return {
    id: record.id,
    generation: validGeneration(record.generation) ? record.generation + 1 : 0,
  };
}

/** Canonical identity for one participant installation at one Trellis origin. */
export function browserInstallationScope(
  trellisUrl: string,
  participantId: string,
): string {
  return JSON.stringify([
    "trellis.browser-installation.v2",
    new URL(trellisUrl).origin,
    participantId,
  ]);
}

/** Persists only a browser installation key and nullable login metadata. */
export class BrowserSessionStore {
  readonly #id: string;
  readonly #temporary: boolean;

  constructor(
    scope: string,
    persistence: "remembered" | "temporary" = "remembered",
  ) {
    if (!scope.trim() || scope.length > 4_096) {
      throw new Error("browser installation scope is invalid");
    }
    this.#id = scope;
    this.#temporary = persistence === "temporary";
  }

  async getOrCreateCredential(
    now = Date.now(),
  ): Promise<BrowserSessionCredential> {
    const candidateSeed = crypto.getRandomValues(new Uint8Array(32));
    const candidateSessionKey = publicKeyBase64urlFromSeed(candidateSeed);
    if (this.#temporary) {
      let current = temporaryInstallations.get(this.#id);
      if (current && expired(current, now)) {
        current = tombstone(current);
        temporaryInstallations.set(this.#id, current);
      }
      const existing = credential(current);
      if (existing) return existing;
      const created = {
        id: this.#id,
        generation: validGeneration(current?.generation)
          ? current.generation
          : 0,
        seed: candidateSeed,
        sessionKey: candidateSessionKey,
      };
      temporaryInstallations.set(this.#id, created);
      return credential(created)!;
    }
    const db = await openDB();
    return await new Promise((resolve, reject) => {
      let result: BrowserSessionCredential;
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const request = store.get(this.#id);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        let current = request.result as BrowserInstallationRecord | undefined;
        if (current && expired(current, now)) current = tombstone(current);
        const existing = credential(current);
        if (existing) {
          result = existing;
          if (current !== request.result) store.put(current);
          return;
        }
        const created = {
          id: this.#id,
          generation: validGeneration(current?.generation)
            ? current.generation
            : 0,
          seed: candidateSeed,
          sessionKey: candidateSessionKey,
        };
        store.put(created);
        result = credential(created)!;
      };
      tx.oncomplete = () => {
        db.close();
        resolve(result);
      };
      tx.onerror = () => reject(tx.error);
    });
  }

  async readLogin(
    now = Date.now(),
  ): Promise<BrowserSessionCredential | undefined> {
    if (this.#temporary) {
      const current = temporaryInstallations.get(this.#id);
      if (!current) return undefined;
      if (expired(current, now)) {
        temporaryInstallations.set(this.#id, tombstone(current));
        return undefined;
      }
      return credential(current);
    }
    const db = await openDB();
    return await new Promise((resolve, reject) => {
      let result: BrowserSessionCredential | undefined;
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const request = store.get(this.#id);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const current = request.result as BrowserInstallationRecord | undefined;
        if (current && expired(current, now)) {
          store.put(tombstone(current));
          return;
        }
        result = credential(current);
      };
      tx.oncomplete = () => {
        db.close();
        resolve(result);
      };
      tx.onerror = () => reject(tx.error);
    });
  }

  rememberFlow(
    expected: Pick<BrowserSessionCredential, "generation" | "sessionKey">,
    flowId: string,
  ): Promise<boolean> {
    return this.#update((current) => {
      if (
        current.generation !== expected.generation ||
        current.sessionKey !== expected.sessionKey
      ) return undefined;
      return { ...current, pendingFlowId: flowId };
    });
  }

  completeBind(
    expected: Pick<
      BrowserSessionCredential,
      "generation" | "sessionKey" | "pendingFlowId"
    >,
    login: { loginSessionId: string; expiresAt: number | null },
  ): Promise<boolean> {
    return this.#update((current) => {
      if (
        current.generation !== expected.generation ||
        current.sessionKey !== expected.sessionKey ||
        !expected.pendingFlowId ||
        current.pendingFlowId !== expected.pendingFlowId
      ) return undefined;
      const { pendingFlowId: _, ...retained } = current;
      return { ...retained, ...login };
    });
  }

  clearLogin(
    expected: Pick<BrowserSessionCredential, "generation" | "sessionKey"> & {
      loginSessionId?: string;
    },
  ): Promise<boolean> {
    return this.#update((current) => {
      if (
        current.generation !== expected.generation ||
        current.sessionKey !== expected.sessionKey ||
        (expected.loginSessionId !== undefined &&
          current.loginSessionId !== expected.loginSessionId)
      ) return undefined;
      return tombstone(current);
    });
  }

  async #update(
    update: (
      current: BrowserInstallationRecord,
    ) => BrowserInstallationRecord | undefined,
  ): Promise<boolean> {
    if (this.#temporary) {
      const current = temporaryInstallations.get(this.#id);
      if (!current) return false;
      const next = update(current);
      if (!next) return false;
      temporaryInstallations.set(this.#id, next);
      return true;
    }
    const db = await openDB();
    return await new Promise((resolve, reject) => {
      let changed = false;
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const request = store.get(this.#id);
      request.onerror = () => reject(request.error);
      request.onsuccess = () => {
        const current = request.result as BrowserInstallationRecord | undefined;
        if (!current) return;
        const next = update(current);
        if (!next) return;
        store.put(next);
        changed = true;
      };
      tx.oncomplete = () => {
        db.close();
        resolve(changed);
      };
      tx.onerror = () => reject(tx.error);
    });
  }
}
