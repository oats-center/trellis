import { err, isErr, ok, type Result } from "@oatscenter/result";
import { AuthError } from "../../errors/AuthError.ts";

/** Exact ordinary-app and runtime identity owning one browser OAuth record. */
export interface BrowserOAuthScope {
  readonly issuer: string;
  readonly instanceId: string;
  readonly account: string;
  readonly clientId: string;
  readonly browserOrigin: string;
  readonly credentialHandle: string;
}

/** Identity returned by the verified successful OAuth admission response. */
export interface BrowserOAuthGrant {
  readonly principalId: string;
  readonly oauthGrantId: string;
  readonly loginSessionId: string;
  readonly refreshToken: string;
}

interface OAuthRecord {
  id: string;
  revision: number;
  dpop: CryptoKeyPair;
  wrapping: CryptoKey;
  iv?: Uint8Array<ArrayBuffer>;
  ciphertext?: ArrayBuffer;
  grant?: Omit<BrowserOAuthGrant, "refreshToken">;
  recoveryRequired?: boolean;
}

function requestValue<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

function committed(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error);
    transaction.onerror = () => reject(transaction.error);
  });
}

async function recordTransaction<T>(
  database: IDBDatabase,
  mode: IDBTransactionMode,
  operation: (store: IDBObjectStore) => Promise<T>,
): Promise<T> {
  const transaction = database.transaction("credentials", mode);
  const finished = committed(transaction);
  const [result] = await Promise.all([
    operation(transaction.objectStore("credentials")),
    finished,
  ]);
  return result;
}

/**
 * Owns a non-extractable persistent DPoP key and an independently non-extractable
 * AES-GCM wrapping key in the ordinary application's IndexedDB realm. Refresh
 * credentials are encrypted and bound to the exact selected scope. Ephemeral
 * NATS keys and SessionAuthorities are never persisted here. The retired login
 * installation database is neither read nor migrated.
 */
export class BrowserSessionStore {
  readonly #database: IDBDatabase;
  readonly #scope: string;
  #record: OAuthRecord;

  private constructor(
    database: IDBDatabase,
    scope: string,
    record: OAuthRecord,
  ) {
    this.#database = database;
    this.#scope = scope;
    this.#record = record;
  }

  /** Open or create custody for this ordinary app, without temporary fallback. */
  static async open(
    scope: BrowserOAuthScope,
  ): Promise<Result<BrowserSessionStore, AuthError>> {
    if (scope.browserOrigin !== globalThis.location.origin) {
      return err(new AuthError({ reason: "InvalidCredential" }));
    }
    if (
      !globalThis.navigator?.locks ||
      !globalThis.indexedDB ||
      !globalThis.crypto?.subtle
    ) {
      return err(new AuthError({ reason: "UnsupportedCredentialStorage" }));
    }
    if (
      !/^[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(scope.credentialHandle) ||
      [scope.issuer, scope.instanceId, scope.account, scope.clientId].some(
        (value) => !value || value.length > 4096,
      )
    )
      return err(new AuthError({ reason: "InvalidCredential" }));
    const identity = JSON.stringify([
      scope.issuer,
      scope.instanceId,
      scope.account,
      scope.clientId,
      scope.browserOrigin,
      scope.credentialHandle,
    ]);
    let database: IDBDatabase | undefined;
    try {
      const opening = indexedDB.open("trellis-oauth", 1);
      opening.onupgradeneeded = () => {
        opening.result.createObjectStore("credentials", { keyPath: "id" });
      };
      database = await requestValue(opening);
      database.onversionchange = () => database?.close();
      let record: OAuthRecord | undefined = await recordTransaction(
        database,
        "readonly",
        (store) => requestValue(store.get(identity)),
      );
      if (!record) {
        const dpop = await crypto.subtle.generateKey(
          { name: "ECDSA", namedCurve: "P-256" },
          false,
          ["sign", "verify"],
        );
        const wrapping = await crypto.subtle.generateKey(
          { name: "AES-GCM", length: 256 },
          false,
          ["encrypt", "decrypt"],
        );
        record = await recordTransaction(
          database,
          "readwrite",
          async (store) => {
            // A competing tab's existing key and family must never be replaced.
            let winner: OAuthRecord | undefined = await requestValue(
              store.get(identity),
            );
            if (!winner) {
              winner = { id: identity, revision: 0, dpop, wrapping };
              await requestValue(store.add(winner));
            }
            return winner;
          },
        );
      }
      if (
        record.id !== identity ||
        !Number.isSafeInteger(record.revision) ||
        record.revision < 0 ||
        !(record.dpop?.privateKey instanceof CryptoKey) ||
        !(record.dpop.publicKey instanceof CryptoKey) ||
        record.dpop.privateKey.extractable ||
        !(record.wrapping instanceof CryptoKey) ||
        record.wrapping.extractable
      ) {
        database.close();
        return err(new AuthError({ reason: "InvalidCredential" }));
      }
      return ok(new BrowserSessionStore(database, identity, record));
    } catch {
      database?.close();
      return err(new AuthError({ reason: "StorageUnavailable" }));
    }
  }

  /** Non-extractable signing handles for the standards client's DPoP adapter. */
  get dpop(): Readonly<CryptoKeyPair> {
    return { ...this.#record.dpop };
  }

  /** Decrypt this exact scope's refresh credential into transient SDK memory. */
  async refreshToken(): Promise<Result<string | undefined, AuthError>> {
    if (this.#record.recoveryRequired) {
      return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
    }
    const { iv, ciphertext } = this.#record;
    if (!iv && !ciphertext) return ok(undefined);
    if (!iv || !ciphertext) {
      return err(new AuthError({ reason: "InvalidCredential" }));
    }
    try {
      const bytes = await crypto.subtle.decrypt(
        {
          name: "AES-GCM",
          iv,
          additionalData: new TextEncoder().encode(this.#scope),
        },
        this.#record.wrapping,
        ciphertext,
      );
      const token = new TextDecoder("utf-8", { fatal: true }).decode(bytes);
      if (!token) return err(new AuthError({ reason: "InvalidCredential" }));
      return ok(token);
    } catch {
      return err(new AuthError({ reason: "InvalidCredential" }));
    }
  }

  /**
   * Commit rotation only if this store still owns the read revision. A stale tab
   * cannot overwrite another tab's newly rotated family or a cleared login.
   */
  async #saveRefreshToken(
    token: string | undefined,
    grant: OAuthRecord["grant"] = this.#record.grant,
    recoveryRequired = false,
  ): Promise<Result<void, AuthError>> {
    try {
      const iv =
        token === undefined
          ? undefined
          : crypto.getRandomValues(new Uint8Array(12));
      const ciphertext =
        iv === undefined
          ? undefined
          : await crypto.subtle.encrypt(
              {
                name: "AES-GCM",
                iv,
                additionalData: new TextEncoder().encode(this.#scope),
              },
              this.#record.wrapping,
              new TextEncoder().encode(token),
            );
      const replacement = await recordTransaction(
        this.#database,
        "readwrite",
        async (store) => {
          const current: OAuthRecord | undefined = await requestValue(
            store.get(this.#scope),
          );
          if (!current || current.revision !== this.#record.revision)
            return undefined;
          const replacement: OAuthRecord = {
            id: this.#scope,
            revision: current.revision + 1,
            dpop: current.dpop,
            wrapping: current.wrapping,
            iv,
            ciphertext,
            grant,
            recoveryRequired,
          };
          await requestValue(store.put(replacement));
          return replacement;
        },
      );
      if (!replacement) return err(new AuthError({ reason: "Conflict" }));
      this.#record = replacement;
      return ok(undefined);
    } catch {
      return err(new AuthError({ reason: "StorageUnavailable" }));
    }
  }

  /** Bind an unused credential handle to the verified user and initial family. */
  async rememberGrant(
    grant: BrowserOAuthGrant,
  ): Promise<Result<void, AuthError>> {
    if (
      !grant.principalId ||
      !grant.oauthGrantId ||
      !grant.loginSessionId ||
      !grant.refreshToken
    ) {
      return err(new AuthError({ reason: "InvalidCredential" }));
    }
    try {
      return await navigator.locks.request(this.#scope, async () => {
        const current: OAuthRecord | undefined = await recordTransaction(
          this.#database,
          "readonly",
          (store) => requestValue(store.get(this.#scope)),
        );
        if (
          !current ||
          (current.grant && current.grant.principalId !== grant.principalId)
        ) {
          return err(new AuthError({ reason: "Conflict" }));
        }
        this.#record = current;
        const { refreshToken, ...identity } = grant;
        return await this.#saveRefreshToken(refreshToken, identity);
      });
    } catch {
      return err(new AuthError({ reason: "StorageUnavailable" }));
    }
  }

  /**
   * One cross-tab owner rereads, exchanges, and commits a refresh. Mark recovery
   * before sending: a lost response must never cause replay of a spent token.
   * Network work runs outside IndexedDB transactions but inside the Web Lock.
   */
  async rotate<T>(
    exchange: (
      token: string,
      dpop: Readonly<CryptoKeyPair>,
    ) => Promise<Result<{ value: T; refreshToken: string }, AuthError>>,
  ): Promise<Result<T, AuthError>> {
    try {
      return await navigator.locks.request(this.#scope, async () => {
        const current: OAuthRecord | undefined = await recordTransaction(
          this.#database,
          "readonly",
          (store) => requestValue(store.get(this.#scope)),
        );
        if (!current || !current.grant) {
          return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
        }
        this.#record = current;
        const token = (await this.refreshToken()).take();
        if (isErr(token)) return err(token.error);
        if (!token) {
          return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
        }
        const marked = await this.#saveRefreshToken(token, current.grant, true);
        if (marked.isErr()) return err(marked.error);
        const rotated = (await exchange(token, this.dpop)).take();
        if (isErr(rotated)) return err(rotated.error);
        if (!rotated.refreshToken) {
          return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
        }
        const saved = await this.#saveRefreshToken(rotated.refreshToken);
        if (saved.isErr()) return err(saved.error);
        return ok(rotated.value);
      });
    } catch {
      return err(new AuthError({ reason: "OAuthRecoveryRequired" }));
    }
  }

  /** Clear this family under the same lock used by competing refresh owners. */
  async clear(): Promise<Result<void, AuthError>> {
    try {
      return await navigator.locks.request(this.#scope, async () => {
        const current: OAuthRecord | undefined = await recordTransaction(
          this.#database,
          "readonly",
          (store) => requestValue(store.get(this.#scope)),
        );
        if (!current) {
          return err(new AuthError({ reason: "InvalidCredential" }));
        }
        this.#record = current;
        return await this.#saveRefreshToken(undefined, undefined, true);
      });
    } catch {
      return err(new AuthError({ reason: "StorageUnavailable" }));
    }
  }

  /** Release this tab's database connection without deleting another tab's state. */
  close(): void {
    this.#database.close();
  }
}
