import { constants, fstatSync, readdirSync, type Stats } from "node:fs";
import { createHash } from "node:crypto";
import { lstat, mkdir, open } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { isIP } from "node:net";
import { DatabaseSync } from "node:sqlite";
import { setTimeout } from "node:timers/promises";
import { z } from "zod";
import {
  SessionAuthorityOwner,
  type RuntimeAuthorizationTrust,
} from "../auth/authority.ts";
import { AuthError } from "../errors/AuthError.ts";
import {
  base64urlDecode,
  base64urlEncode,
  sha256,
  utf8,
} from "../auth/utils.ts";
import { codecs } from "../generated_support.ts";
import { fromPublic } from "@nats-io/nkeys";

/** Exact credential slot, independent of the generated application request. */
export type NativeCredentialScope = Readonly<{
  issuer: string;
  instanceId: string;
  account: string;
  clientId: string;
  credentialHandle: string;
}>;

const trustSchema = z.object({
  instanceId: z.string().min(1),
  account: z.string().min(1),
  issuer: z.object({
    keyId: z.string().min(1),
    publicKey: z.string().min(1),
    state: z.literal("active"),
  }),
  rotationSequence: z.string().regex(/^(0|[1-9][0-9]*)$/),
});
const grantSchema = z.object({
  principalId: z.string().min(1).max(4096),
  oauthGrantId: z.string().min(1).max(4096),
  loginSessionId: z.string().min(1).max(4096),
  refreshToken: z.string().min(1).max(16384),
});

/** Verified token response identity; refresh secrets never leave custody publicly. @internal */
export type NativeOAuthGrant = z.infer<typeof grantSchema>;

// DatabaseSync reopens a pathname and does not expose the resulting descriptor.
// Synchronous before/after descriptor snapshots identify its actual open file,
// including an inode swapped into and then out of the pathname during open.
// No fallback to pathname-only verification is safe at this boundary.
function openSqliteChecked(path: string, validated: Stats): DatabaseSync {
  let descriptorDirectory: string | undefined;
  let before: Set<string> | undefined;
  for (const directory of ["/proc/self/fd", "/dev/fd"]) {
    try {
      before = new Set(
        readdirSync(directory).filter((descriptor) => {
          if (!/^\d+$/.test(descriptor)) return false;
          try {
            fstatSync(Number(descriptor));
            return true;
          } catch (error) {
            if (
              error instanceof Error &&
              "code" in error &&
              error.code === "EBADF"
            )
              return false;
            throw error;
          }
        }),
      );
      descriptorDirectory = directory;
      break;
    } catch {
      /* Try the other OS descriptor filesystem. */
    }
  }
  if (!descriptorDirectory || !before)
    throw new AuthError({ reason: "UnsupportedCredentialStorage" });
  const database = new DatabaseSync(path);
  try {
    const opened: Stats[] = [];
    for (const descriptor of readdirSync(descriptorDirectory)) {
      if (before.has(descriptor) || !/^\d+$/.test(descriptor)) continue;
      try {
        const stat = fstatSync(Number(descriptor));
        if (stat.isFile()) opened.push(stat);
      } catch (error) {
        if (
          !(error instanceof Error) ||
          !("code" in error) ||
          error.code !== "EBADF"
        )
          throw error;
      }
    }
    if (
      opened.length !== 1 ||
      opened[0].dev !== validated.dev ||
      opened[0].ino !== validated.ino ||
      opened[0].uid !== validated.uid ||
      opened[0].nlink !== 1 ||
      (opened[0].mode & 0o077) !== 0
    )
      throw new AuthError({ reason: "UnsafeCredentialStorage" });
    return database;
  } catch (error) {
    database.close();
    throw error;
  }
}

/**
 * POSIX owner-only SQLite custody. The lease transaction and durable record are
 * separate databases. A persisted lease inode detects pathname replacement
 * before another opener can load credentials. The protection boundary is the
 * effective OS user: a process able to rewrite the entire private directory can
 * already read/replace its private key and is not isolated by POSIX modes.
 * SQLite's Node API does not expose its fd or an fd-based open. OS descriptor
 * snapshots additionally verify the actual SQLite inode, failing closed when
 * the descriptor cannot be uniquely identified. Full private-directory rewrite
 * by the owning OS user is not a security isolation boundary.
 * @internal
 */
export class NativeCredentialLease {
  readonly scope: NativeCredentialScope;
  readonly #lease: DatabaseSync;
  readonly #records: DatabaseSync;
  readonly #key: CryptoKey;
  readonly #publicJwk: JsonWebKey;
  #closed = false;

  private constructor(
    scope: NativeCredentialScope,
    lease: DatabaseSync,
    records: DatabaseSync,
    key: CryptoKey,
    publicJwk: JsonWebKey,
  ) {
    this.scope = Object.freeze({ ...scope });
    this.#lease = lease;
    this.#records = records;
    this.#key = key;
    this.#publicJwk = publicJwk;
  }

  /** Acquire an OS-backed exclusive family lease; cancellation never breaks another owner's lock. */
  static async open(
    directory: string,
    selectedScope: NativeCredentialScope,
    initial: RuntimeAuthorizationTrust,
    signal?: AbortSignal,
  ): Promise<NativeCredentialLease> {
    if (
      process.platform === "win32" ||
      !isAbsolute(directory) ||
      typeof process.getuid !== "function"
    )
      throw new AuthError({ reason: "UnsupportedCredentialStorage" });
    const uid = process.getuid();
    const scope = Object.freeze({
      issuer: selectedScope.issuer,
      instanceId: selectedScope.instanceId,
      account: selectedScope.account,
      clientId: selectedScope.clientId,
      credentialHandle: selectedScope.credentialHandle,
    });
    const origin = new URL(scope.issuer);
    const host = origin.hostname.replace(/^\[|\]$/g, "");
    const loopback =
      host === "localhost" ||
      host === "::1" ||
      (isIP(host) === 4 && host.startsWith("127."));
    if (
      origin.origin !== scope.issuer ||
      (origin.protocol !== "https:" &&
        !(origin.protocol === "http:" && loopback))
    )
      throw new AuthError({ reason: "InvalidCredential" });
    codecs.ulid.decode(scope.instanceId);
    codecs.ulid.decode(scope.credentialHandle);
    const account = fromPublic(scope.account);
    if (
      !scope.account.startsWith("A") ||
      account.getPublicKey() !== scope.account
    )
      throw new AuthError({ reason: "InvalidCredential" });
    const check = (stat: Stats, folder: boolean) => {
      if (
        stat.uid !== uid ||
        (stat.mode & 0o077) !== 0 ||
        (folder ? !stat.isDirectory() : !stat.isFile() || stat.nlink !== 1)
      )
        throw new AuthError({ reason: "UnsafeCredentialStorage" });
    };
    if (Object.values(scope).some((value) => !value || value.length > 4096))
      throw new AuthError({ reason: "InvalidCredential" });
    await mkdir(directory, { recursive: true, mode: 0o700 });
    check(await lstat(directory), true);
    for (let parent = dirname(directory); ; parent = dirname(parent)) {
      const stat = await lstat(parent);
      if (
        !stat.isDirectory() ||
        (stat.uid !== uid && stat.uid !== 0) ||
        ((stat.mode & 0o022) !== 0 && (stat.mode & 0o1000) === 0)
      )
        throw new AuthError({ reason: "UnsafeCredentialStorage" });
      if (parent === dirname(parent)) break;
    }
    const identity = base64urlEncode(
      await sha256(
        utf8(
          JSON.stringify([
            scope.issuer,
            scope.instanceId,
            scope.account,
            scope.clientId,
            scope.credentialHandle,
          ]),
        ),
      ),
    );
    const slot = join(directory, identity);
    await mkdir(slot, { mode: 0o700 }).catch((error: NodeJS.ErrnoException) => {
      if (error.code !== "EEXIST") throw error;
    });
    check(await lstat(slot), true);
    const validated: Stats[] = [];
    let lease: DatabaseSync | undefined;
    let records: DatabaseSync | undefined;
    try {
      for (const name of ["lease.sqlite", "credential.sqlite"]) {
        const path = join(slot, name);
        try {
          const created = await open(
            path,
            constants.O_CREAT |
              constants.O_EXCL |
              constants.O_RDWR |
              constants.O_NOFOLLOW,
            0o600,
          );
          // Only close our exclusively created file; opening/closing an existing
          // inode here can drop another SQLite connection's POSIX process lock.
          await created.close();
        } catch (error) {
          if (
            !(error instanceof Error) ||
            !("code" in error) ||
            error.code !== "EEXIST"
          )
            throw error;
        }
        const stat = await lstat(path);
        check(stat, false);
        validated.push(stat);
      }
      lease = openSqliteChecked(join(slot, "lease.sqlite"), validated[0]);
      while (true) {
        signal?.throwIfAborted();
        try {
          lease.exec("BEGIN IMMEDIATE");
          break;
        } catch (error) {
          if (
            !(error instanceof Error) ||
            !("errcode" in error) ||
            error.errcode !== 5
          )
            throw error;
          await setTimeout(25, undefined, { signal });
        }
      }
      records = openSqliteChecked(
        join(slot, "credential.sqlite"),
        validated[1],
      );
      for (let index = 0; index < validated.length; index++) {
        const before = validated[index];
        const after = await lstat(
          join(slot, index === 0 ? "lease.sqlite" : "credential.sqlite"),
        );
        check(after, false);
        if (before.dev !== after.dev || before.ino !== after.ino)
          throw new AuthError({ reason: "UnsafeCredentialStorage" });
      }
      // Recheck pathname identity as well as the actual SQLite descriptors above.
      records.exec(
        "PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA; CREATE TABLE IF NOT EXISTS credential (id INTEGER PRIMARY KEY CHECK(id=1), scope TEXT NOT NULL, private_jwk TEXT NOT NULL, trust TEXT NOT NULL, grant_json TEXT, spent INTEGER NOT NULL CHECK(spent IN (0,1)), lease_identity TEXT NOT NULL)",
      );
      let row = records.prepare("SELECT * FROM credential WHERE id=1").get();
      const scopeJson = JSON.stringify(scope);
      const leaseStat = validated[0];
      const leaseIdentity = `${leaseStat.dev}:${leaseStat.ino}`;
      if (row && row.lease_identity !== leaseIdentity)
        throw new AuthError({ reason: "ReplacedCredentialLease" });
      if (!row) {
        const issuerBytes = base64urlDecode(initial.issuer.publicKey);
        if (
          issuerBytes.length !== 32 ||
          base64urlEncode(issuerBytes) !== initial.issuer.publicKey ||
          base64urlEncode(await sha256(issuerBytes)) !== initial.issuer.keyId
        )
          throw new AuthError({ reason: "InvalidCredential" });
        await crypto.subtle.importKey(
          "raw",
          new Uint8Array(issuerBytes),
          { name: "Ed25519" },
          false,
          ["verify"],
        );
        const pair = await crypto.subtle.generateKey(
          { name: "ECDSA", namedCurve: "P-256" },
          true,
          ["sign", "verify"],
        );
        const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
        trustSchema.parse(initial);
        if (
          scope.instanceId !== initial.instanceId ||
          scope.account !== initial.account
        )
          throw new AuthError({ reason: "InvalidCredential" });
        records
          .prepare("INSERT INTO credential VALUES (1, ?, ?, ?, NULL, 0, ?)")
          .run(
            scopeJson,
            JSON.stringify(jwk),
            JSON.stringify(initial),
            leaseIdentity,
          );
        row = records.prepare("SELECT * FROM credential WHERE id=1").get();
      }
      if (
        !row ||
        row.scope !== scopeJson ||
        typeof row.private_jwk !== "string"
      )
        throw new AuthError({ reason: "InvalidCredential" });
      const jwk = z
        .object({
          kty: z.literal("EC"),
          crv: z.literal("P-256"),
          x: z.string(),
          y: z.string(),
          d: z.string(),
        })
        .parse(JSON.parse(row.private_jwk));
      const key = await crypto.subtle.importKey(
        "jwk",
        jwk,
        { name: "ECDSA", namedCurve: "P-256" },
        false,
        ["sign"],
      );
      const result = new NativeCredentialLease(scope, lease, records, key, {
        kty: jwk.kty,
        crv: jwk.crv,
        x: jwk.x,
        y: jwk.y,
      });
      void result.trust;
      signal?.throwIfAborted();
      return result;
    } catch (error) {
      records?.close();
      lease?.close();
      throw error;
    }
  }

  /** Restored issuer pin; discovery cannot overwrite a retained trust floor. */
  get trust(): RuntimeAuthorizationTrust {
    this.#assertOpen();
    const row = this.#records
      .prepare("SELECT trust FROM credential WHERE id=1")
      .get();
    if (typeof row?.trust !== "string")
      throw new AuthError({ reason: "InvalidCredential" });
    const trust = trustSchema.parse(JSON.parse(row.trust));
    const issuerBytes = base64urlDecode(trust.issuer.publicKey);
    if (
      trust.instanceId !== this.scope.instanceId ||
      trust.account !== this.scope.account ||
      issuerBytes.length !== 32 ||
      base64urlEncode(issuerBytes) !== trust.issuer.publicKey ||
      createHash("sha256").update(issuerBytes).digest("base64url") !==
        trust.issuer.keyId
    )
      throw new AuthError({ reason: "InvalidCredential" });
    return trust;
  }

  /** Retain only trust already authenticated by the common authority owner. */
  retainTrust(owner: SessionAuthorityOwner): void {
    const trust = SessionAuthorityOwner.prototype.trustForCredential.call(
      owner,
      this.trust,
    );
    const old = this.trust;
    trustSchema.parse(trust);
    const issuerBytes = base64urlDecode(trust.issuer.publicKey);
    if (
      issuerBytes.length !== 32 ||
      base64urlEncode(issuerBytes) !== trust.issuer.publicKey ||
      createHash("sha256").update(issuerBytes).digest("base64url") !==
        trust.issuer.keyId
    )
      throw new AuthError({ reason: "InvalidCredential" });
    if (
      trust.instanceId !== old.instanceId ||
      trust.account !== old.account ||
      BigInt(trust.rotationSequence) < BigInt(old.rotationSequence) ||
      (trust.rotationSequence === old.rotationSequence &&
        (trust.issuer.keyId !== old.issuer.keyId ||
          trust.issuer.publicKey !== old.issuer.publicKey ||
          trust.issuer.state !== old.issuer.state))
    )
      throw new AuthError({ reason: "InvalidCredential" });
    this.#records
      .prepare("UPDATE credential SET trust=? WHERE id=1")
      .run(JSON.stringify(trust));
  }

  /** Public RFC 7517 signing key, without private material. */
  get publicJwk(): Readonly<JsonWebKey> {
    return { ...this.#publicJwk };
  }

  /** Sign ES256 JWS input using the non-extractable in-memory key. */
  async sign(input: Uint8Array<ArrayBuffer>): Promise<ArrayBuffer> {
    this.#assertOpen();
    return await crypto.subtle.sign(
      { name: "ECDSA", hash: "SHA-256" },
      this.#key,
      input,
    );
  }

  /** Whether custody holds a known unspent refresh credential, without reading its secret. */
  get refreshAvailable(): boolean {
    this.#assertOpen();
    return (
      this.#records
        .prepare(
          "SELECT id FROM credential WHERE id=1 AND spent=0 AND grant_json IS NOT NULL",
        )
        .get() !== undefined
    );
  }

  /** Commit the spent marker before returning a token to the internal HTTP exchange. */
  beginRotation(): string {
    this.#assertOpen();
    const row = this.#records
      .prepare("SELECT spent, grant_json FROM credential WHERE id=1")
      .get();
    if (!row || row.spent !== 0 || typeof row.grant_json !== "string")
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    const grant = grantSchema.parse(JSON.parse(row.grant_json));
    this.#records.prepare("UPDATE credential SET spent=1 WHERE id=1").run();
    return grant.refreshToken;
  }

  /** Preserve a newly returned, unspent refresh secret without installing admission authority. */
  retainRotatedToken(refreshToken: string): void {
    this.#assertOpen();
    const row = this.#records
      .prepare("SELECT spent, grant_json FROM credential WHERE id=1")
      .get();
    if (typeof row?.grant_json !== "string") return;
    if (row.spent !== 1)
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    const grant = grantSchema.parse(JSON.parse(row.grant_json));
    if (grant.refreshToken === refreshToken)
      throw new AuthError({ reason: "OAuthRecoveryRequired" });
    grant.refreshToken = refreshToken;
    grantSchema.parse(grant);
    this.#records
      .prepare("UPDATE credential SET grant_json=?, spent=0 WHERE id=1")
      .run(JSON.stringify(grant));
  }

  /** Install a fully verified response atomically, preserving the principal on reauthorization. */
  install(grant: NativeOAuthGrant): void {
    this.#assertOpen();
    grantSchema.parse(grant);
    const row = this.#records
      .prepare("SELECT grant_json FROM credential WHERE id=1")
      .get();
    if (
      typeof row?.grant_json === "string" &&
      grantSchema.parse(JSON.parse(row.grant_json)).principalId !==
        grant.principalId
    )
      throw new AuthError({ reason: "InvalidCredential" });
    this.#records
      .prepare("UPDATE credential SET grant_json=?, spent=0 WHERE id=1")
      .run(JSON.stringify(grant));
  }

  /** Release custody only after the session has settled its exchanges. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#records.close();
    this.#lease.close();
  }

  #assertOpen(): void {
    if (this.#closed) throw new AuthError({ reason: "OAuthRecoveryRequired" });
  }
}
