import {
  type AsyncResult,
  type BaseError,
  isErr,
  type UnexpectedError,
} from "@oats-center/result";
import { type StaticDecode, Type } from "typebox";

import type { TypedKV } from "../kv.ts";
import { recordTrellisError } from "../telemetry/mod.ts";
import type { PreparedTrellisEvent } from "../session.ts";

export type OutboxMessageState =
  | "pending"
  | "claimed"
  | "dispatched"
  | "failed";
const outboxClaimMs = 30_000;

export type OutboxRecordKind = "event.publish" | "job.create" | "job.submit";

export type PreparedOutboxRecord = {
  readonly id: string;
  readonly kind: OutboxRecordKind;
  readonly name: string;
  readonly subject: string;
  readonly payload: string;
  readonly headers: Readonly<Record<string, string>>;
};

export type OutboxMessage = {
  id: string;
  kind: OutboxRecordKind;
  name: string;
  subject: string;
  payload: string;
  headers: Record<string, string>;
  state: OutboxMessageState;
  attempts: number;
  createdAt: string;
  updatedAt: string;
  nextAttemptAt?: string;
  lastError?: string;
  outcome?: unknown;
};

/** Raised when an outbox ID is reused for different immutable content. */
export class OutboxDuplicateIdentityError extends Error {
  constructor(readonly id: string) {
    super(
      `Outbox message '${id}' already exists with different immutable content`,
    );
    this.name = "OutboxDuplicateIdentityError";
  }
}

/** Raised when storage reports a duplicate but the winning row cannot be read. */
export class OutboxStorageConflictError extends Error {
  constructor(readonly id: string) {
    super(`Outbox message '${id}' conflicted but no stored row was found`);
    this.name = "OutboxStorageConflictError";
  }
}

export type OutboxDispatchResult = {
  dispatched: number;
  failed: number;
};

/** Queue-admission result persisted after a SQL-outboxed job is dispatched. */
export type OutboxJobDispatchOutcome = {
  readonly kind: string;
  readonly [key: string]: unknown;
};

/** Options for {@link OutboxDispatcher}. */
export type OutboxDispatcherOptions = {
  /** Maximum number of messages claimed by each `dispatchOutbox` batch. */
  limit?: number;
  /** Delay before failed messages become eligible; values below 1ms become 1ms. */
  retryDelayMs?: number;
  /** Delay used to debounce `notify()` calls before starting a drain. */
  debounceMs?: number;
  /** Optional low-frequency wakeup for missed signals or process restarts. */
  idleRetryMs?: number;
  /** Receives repository or publish errors raised by background dispatch. */
  onError?: (error: unknown) => void;
};

export type OutboxDispatchRuntime = {
  /** Dispatches an event-publish outbox record. Returns Void on success, err on transient failure. */
  publishPreparedEvent(
    event: PreparedTrellisEvent,
  ): AsyncResult<void, UnexpectedError>;
  /** Dispatches a job create/submit outbox record and returns its queue-admission outcome. */
  dispatchJobSubmission?(
    message: OutboxMessage,
  ): AsyncResult<OutboxJobDispatchOutcome, BaseError>;
};

export type OutboxRepository = {
  enqueue(record: PreparedOutboxRecord): Promise<OutboxMessage>;
  get(id: string): Promise<OutboxMessage | undefined>;
  /** Claim due work for 30 seconds; the returned attempt fences completion. */
  claimDue(limit: number, now: Date): Promise<OutboxMessage[]>;
  /** Complete the returned claim; false means its ownership was superseded. */
  markDispatched(
    claim: OutboxMessage,
    now: Date,
    outcome?: unknown,
  ): Promise<boolean>;
  /** Retry the returned claim; false means its ownership was superseded. */
  markFailed(
    claim: OutboxMessage,
    failure: { error: string; nextAttemptAt: Date; now: Date },
  ): Promise<boolean>;
};

export type InboxRepository = {
  /** Insert a dedupe marker; true only when this call inserted it. */
  record(messageId: string, now?: Date): Promise<boolean>;
};

/** SQL dialects supported by Trellis SQL outbox helpers. */
export type SqlDialect = "sqlite" | "postgres";

/** Table names used by Trellis SQL outbox and inbox helper tables. */
export type SqlOutboxTables = {
  outbox: string;
  inbox: string;
};

/** Minimal SQL execution surface used by Trellis SQL outbox repositories. */
export type SqlExecutor = {
  query(sql: string, params: readonly unknown[]): Promise<readonly SqlRow[]>;
  execute(sql: string, params: readonly unknown[]): Promise<void>;
};

/** Row shape returned by a {@link SqlExecutor}. */
export type SqlRow = Record<string, unknown>;

/** SQL outbox adapter bundle over caller-owned SQL execution. */
export type SqlOutboxAdapter = {
  outbox: SqlOutboxRepository;
  inbox: SqlInboxRepository;
  ddl: readonly string[];
};

/** Versioned Trellis-owned SQL outbox/inbox migration artifact. */
export type SqlOutboxMigration = {
  /** Stable migration identifier for the dialect and schema version. */
  readonly id: string;
  /** Monotonic Trellis helper-table schema version. */
  readonly version: number;
  /** SQL dialect targeted by this migration artifact. */
  readonly dialect: SqlDialect;
  /** SQL statements that apply this migration. */
  readonly up: readonly string[];
  /** SQL statements that revert this migration when supported by the runner. */
  readonly down?: readonly string[];
  /** Deterministic checksum over the canonical migration SQL. */
  readonly checksum: string;
};

/** Options for generating Trellis SQL outbox/inbox migration artifacts. */
export type SqlOutboxMigrationOptions = {
  /** SQL dialect targeted by the generated migration artifacts. */
  readonly dialect: SqlDialect;
  /** Optional helper-table names; omitted names use Trellis defaults. */
  readonly tables?: Partial<SqlOutboxTables>;
};

export type OutboxKvStore = Pick<
  TypedKV<KvOutboxRecord>,
  "create" | "getEntry" | "replace" | "keys"
>;

/** Default Trellis helper-table names for SQL outbox and inbox storage. */
export const defaultSqlOutboxTables: SqlOutboxTables = Object.freeze({
  outbox: "trellis_outbox",
  inbox: "trellis_inbox",
});

const sqlIdentifierPattern = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Creates SQL outbox/inbox repositories plus current helper-table DDL.
 *
 * Use {@link getSqlOutboxMigrations} when services need versioned Trellis-owned
 * migration artifacts for their normal database migration tooling.
 */
export function createSqlOutboxAdapter(
  executor: SqlExecutor,
  dialect: SqlDialect,
  tables: SqlOutboxTables = defaultSqlOutboxTables,
): SqlOutboxAdapter {
  const sqlTables = validateSqlOutboxTables(tables);
  return {
    outbox: new SqlOutboxRepository(executor, dialect, sqlTables),
    inbox: new SqlInboxRepository(executor, dialect, sqlTables),
    ddl: dialect === "postgres"
      ? createPostgresOutboxSchema(sqlTables)
      : createSqliteOutboxSchema(sqlTables),
  };
}

/**
 * Returns Trellis-owned SQL outbox/inbox migration artifacts for a dialect.
 *
 * Trellis generates the helper-table SQL, but services remain responsible for
 * running these artifacts through their normal database migration tooling.
 */
export function getSqlOutboxMigrations(
  options: SqlOutboxMigrationOptions,
): readonly SqlOutboxMigration[] {
  const tables = resolveSqlOutboxTables(options.tables);
  const up = options.dialect === "postgres"
    ? createPostgresOutboxSchema(tables)
    : createSqliteOutboxSchema(tables);
  const down = createSqlOutboxDownSchema(tables);
  return Object.freeze([
    Object.freeze({
      id: `trellis_sql_outbox_inbox_${options.dialect}_v1`,
      version: 1,
      dialect: options.dialect,
      up,
      down,
      checksum: checksumMigrationSql(up, down),
    }),
  ]);
}

/** Returns SQLite DDL for Trellis outbox and inbox tables. */
export function createSqliteOutboxSchema(
  tables: SqlOutboxTables = defaultSqlOutboxTables,
): readonly string[] {
  const sqlTables = validateSqlOutboxTables(tables);
  return [
    `CREATE TABLE IF NOT EXISTS ${sqlTables.outbox} (id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, subject TEXT NOT NULL, payload TEXT NOT NULL, headers TEXT NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, next_attempt_at TEXT, last_error TEXT, outcome TEXT)`,
    `CREATE INDEX IF NOT EXISTS ${sqlTables.outbox}_due_idx ON ${sqlTables.outbox} (state, next_attempt_at)`,
    `CREATE TABLE IF NOT EXISTS ${sqlTables.inbox} (message_id TEXT PRIMARY KEY, received_at TEXT NOT NULL)`,
  ];
}

/** Returns Postgres DDL for Trellis outbox and inbox tables. */
export function createPostgresOutboxSchema(
  tables: SqlOutboxTables = defaultSqlOutboxTables,
): readonly string[] {
  const sqlTables = validateSqlOutboxTables(tables);
  return [
    `CREATE TABLE IF NOT EXISTS ${sqlTables.outbox} (id text PRIMARY KEY, kind text NOT NULL, name text NOT NULL, subject text NOT NULL, payload text NOT NULL, headers jsonb NOT NULL, state text NOT NULL, attempts integer NOT NULL, created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL, next_attempt_at timestamptz, last_error text, outcome jsonb)`,
    `CREATE INDEX IF NOT EXISTS ${sqlTables.outbox}_due_idx ON ${sqlTables.outbox} (state, next_attempt_at)`,
    `CREATE TABLE IF NOT EXISTS ${sqlTables.inbox} (message_id text PRIMARY KEY, received_at timestamptz NOT NULL)`,
  ];
}

function createSqlOutboxDownSchema(
  tables: SqlOutboxTables,
): readonly string[] {
  const sqlTables = validateSqlOutboxTables(tables);
  return [
    `DROP INDEX IF EXISTS ${sqlTables.outbox}_due_idx`,
    `DROP TABLE IF EXISTS ${sqlTables.inbox}`,
    `DROP TABLE IF EXISTS ${sqlTables.outbox}`,
  ];
}

function resolveSqlOutboxTables(
  tables: Partial<SqlOutboxTables> | undefined,
): SqlOutboxTables {
  return validateSqlOutboxTables({
    outbox: tables?.outbox ?? defaultSqlOutboxTables.outbox,
    inbox: tables?.inbox ?? defaultSqlOutboxTables.inbox,
  });
}

function validateSqlOutboxTables(tables: SqlOutboxTables): SqlOutboxTables {
  validateSqlIdentifier("outbox", tables.outbox);
  validateSqlIdentifier("inbox", tables.inbox);
  return Object.freeze({ outbox: tables.outbox, inbox: tables.inbox });
}

function validateSqlIdentifier(
  kind: keyof SqlOutboxTables,
  name: string,
): void {
  if (!sqlIdentifierPattern.test(name)) {
    throw new Error(
      `Invalid SQL ${kind} table name "${name}". Use a simple identifier matching ${sqlIdentifierPattern.source}.`,
    );
  }
}

function checksumMigrationSql(
  up: readonly string[],
  down: readonly string[],
): string {
  return `fnv1a64:${
    fnv1a64Hex([
      "-- up",
      ...up,
      "-- down",
      ...down,
    ].join("\n"))
  }`;
}

function fnv1a64Hex(input: string): string {
  let hash = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  for (const character of input) {
    hash ^= BigInt(character.codePointAt(0) ?? 0);
    hash = BigInt.asUintN(64, hash * prime);
  }
  return hash.toString(16).padStart(16, "0");
}

/** In-memory outbox repository intended for tests and local process adapters. */
export class MemoryOutboxRepository implements OutboxRepository {
  #messages = new Map<string, OutboxMessage>();

  async enqueue(record: PreparedOutboxRecord): Promise<OutboxMessage> {
    const now = new Date().toISOString();
    const id = record.id;
    const existing = this.#messages.get(id);
    if (existing) {
      assertOutboxIdentity(existing, record);
      return { ...existing, headers: { ...existing.headers } };
    }
    const message: OutboxMessage = {
      id,
      kind: record.kind,
      name: record.name,
      subject: record.subject,
      payload: record.payload,
      headers: { ...record.headers },
      state: "pending",
      attempts: 0,
      createdAt: now,
      updatedAt: now,
    };
    this.#messages.set(id, message);
    return message;
  }

  get(id: string): Promise<OutboxMessage | undefined> {
    const message = this.#messages.get(id);
    return Promise.resolve(
      message ? { ...message, headers: { ...message.headers } } : undefined,
    );
  }

  async claimDue(limit: number, now: Date): Promise<OutboxMessage[]> {
    const dueAt = now.toISOString();
    const claimed: OutboxMessage[] = [];
    for (const message of this.#messages.values()) {
      if (claimed.length >= limit) break;
      if (message.state === "dispatched") continue;
      if (
        message.nextAttemptAt !== undefined && message.nextAttemptAt > dueAt
      ) {
        continue;
      }
      message.state = "claimed";
      message.attempts += 1;
      message.updatedAt = dueAt;
      message.nextAttemptAt = new Date(now.getTime() + outboxClaimMs)
        .toISOString();
      claimed.push({ ...message, headers: { ...message.headers } });
    }
    return claimed;
  }

  async markDispatched(
    claim: OutboxMessage,
    now: Date,
    outcome?: unknown,
  ): Promise<boolean> {
    const message = this.#messages.get(claim.id);
    if (message?.state !== "claimed" || message.attempts !== claim.attempts) {
      return false;
    }
    this.#messages.set(claim.id, {
      ...message,
      state: "dispatched",
      updatedAt: now.toISOString(),
      nextAttemptAt: undefined,
      lastError: undefined,
      outcome: outcome ?? message.outcome,
    });
    return true;
  }

  async markFailed(
    claim: OutboxMessage,
    failure: { error: string; nextAttemptAt: Date; now: Date },
  ): Promise<boolean> {
    const message = this.#messages.get(claim.id);
    if (message?.state !== "claimed" || message.attempts !== claim.attempts) {
      return false;
    }
    this.#messages.set(claim.id, {
      ...message,
      state: "failed",
      updatedAt: failure.now.toISOString(),
      nextAttemptAt: failure.nextAttemptAt.toISOString(),
      lastError: failure.error,
    });
    return true;
  }

  snapshot(): readonly OutboxMessage[] {
    return Array.from(this.#messages.values()).map((message) => ({
      ...message,
      headers: { ...message.headers },
    }));
  }
}

/** In-memory inbox repository intended for duplicate-suppression tests. */
export class MemoryInboxRepository implements InboxRepository {
  #seen = new Set<string>();

  async record(messageId: string): Promise<boolean> {
    if (this.#seen.has(messageId)) return false;
    this.#seen.add(messageId);
    return true;
  }
}

/** SQL-backed outbox repository over a caller-owned executor. */
export class SqlOutboxRepository implements OutboxRepository {
  readonly tables: SqlOutboxTables;

  constructor(
    readonly executor: SqlExecutor,
    readonly dialect: SqlDialect,
    tables: SqlOutboxTables = defaultSqlOutboxTables,
  ) {
    this.tables = validateSqlOutboxTables(tables);
  }

  async enqueue(record: PreparedOutboxRecord): Promise<OutboxMessage> {
    const now = new Date().toISOString();
    const message: OutboxMessage = {
      id: record.id,
      kind: record.kind,
      name: record.name,
      subject: record.subject,
      payload: record.payload,
      headers: { ...record.headers },
      state: "pending",
      attempts: 0,
      createdAt: now,
      updatedAt: now,
    };
    const headers = JSON.stringify(message.headers);
    const conflict = this.dialect === "postgres"
      ? "ON CONFLICT (id) DO NOTHING"
      : "ON CONFLICT(id) DO NOTHING";
    const inserted = await this.executor.query(
      `INSERT INTO ${this.tables.outbox} (id, kind, name, subject, payload, headers, state, attempts, created_at, updated_at, next_attempt_at, last_error, outcome) VALUES (${
        placeholders(this.dialect, 13)
      }) ${conflict} RETURNING *`,
      [
        message.id,
        message.kind,
        message.name,
        message.subject,
        message.payload,
        headers,
        message.state,
        message.attempts,
        message.createdAt,
        message.updatedAt,
        null,
        null,
        null,
      ],
    );
    if (inserted[0]) return rowToOutboxMessage(inserted[0]);
    const existing = await this.get(record.id);
    if (!existing) throw new OutboxStorageConflictError(record.id);
    assertOutboxIdentity(existing, record);
    return existing;
  }

  async get(id: string): Promise<OutboxMessage | undefined> {
    const rows = await this.executor.query(
      `SELECT id, kind, name, subject, payload, headers, state, attempts, created_at, updated_at, next_attempt_at, last_error, outcome FROM ${this.tables.outbox} WHERE id = ${
        placeholder(this.dialect, 1)
      } LIMIT 1`,
      [id],
    );
    return rows[0] ? rowToOutboxMessage(rows[0]) : undefined;
  }

  async claimDue(limit: number, now: Date): Promise<OutboxMessage[]> {
    const rows = await this.executor.query(
      `UPDATE ${this.tables.outbox} SET state = ${
        placeholder(this.dialect, 1)
      }, attempts = attempts + 1, updated_at = ${
        placeholder(this.dialect, 2)
      }, next_attempt_at = ${placeholder(this.dialect, 3)} WHERE id IN (
        SELECT id FROM ${this.tables.outbox} WHERE state != ${
        placeholder(this.dialect, 4)
      }
        AND (next_attempt_at IS NULL OR next_attempt_at <= ${
        placeholder(this.dialect, 5)
      })
        ORDER BY created_at LIMIT ${placeholder(this.dialect, 6)}${
        this.dialect === "postgres" ? " FOR UPDATE SKIP LOCKED" : ""
      }
      ) RETURNING *`,
      [
        "claimed",
        now.toISOString(),
        new Date(now.getTime() + outboxClaimMs).toISOString(),
        "dispatched",
        now.toISOString(),
        Math.max(0, limit),
      ],
    );
    return rows.map(rowToOutboxMessage);
  }

  async markDispatched(
    claim: OutboxMessage,
    now: Date,
    outcome?: unknown,
  ): Promise<boolean> {
    const outcomeJson = outcome !== undefined ? JSON.stringify(outcome) : null;
    const rows = await this.executor.query(
      `UPDATE ${this.tables.outbox} SET state = ${
        placeholder(this.dialect, 1)
      }, updated_at = ${
        placeholder(this.dialect, 2)
      }, next_attempt_at = NULL, last_error = NULL, outcome = ${
        placeholder(this.dialect, 3)
      } WHERE id = ${
        placeholder(this.dialect, 4)
      } AND state = 'claimed' AND attempts = ${
        placeholder(this.dialect, 5)
      } RETURNING id`,
      ["dispatched", now.toISOString(), outcomeJson, claim.id, claim.attempts],
    );
    return rows.length === 1;
  }

  async markFailed(
    claim: OutboxMessage,
    failure: { error: string; nextAttemptAt: Date; now: Date },
  ): Promise<boolean> {
    const rows = await this.executor.query(
      `UPDATE ${this.tables.outbox} SET state = ${
        placeholder(this.dialect, 1)
      }, updated_at = ${placeholder(this.dialect, 2)}, next_attempt_at = ${
        placeholder(this.dialect, 3)
      }, last_error = ${placeholder(this.dialect, 4)} WHERE id = ${
        placeholder(this.dialect, 5)
      } AND state = 'claimed' AND attempts = ${
        placeholder(this.dialect, 6)
      } RETURNING id`,
      [
        "failed",
        failure.now.toISOString(),
        failure.nextAttemptAt.toISOString(),
        failure.error,
        claim.id,
        claim.attempts,
      ],
    );
    return rows.length === 1;
  }
}

/** SQL-backed inbox repository over a caller-owned executor. */
export class SqlInboxRepository implements InboxRepository {
  readonly tables: SqlOutboxTables;

  constructor(
    readonly executor: SqlExecutor,
    readonly dialect: SqlDialect,
    tables: SqlOutboxTables = defaultSqlOutboxTables,
  ) {
    this.tables = validateSqlOutboxTables(tables);
  }

  async record(messageId: string, now: Date = new Date()): Promise<boolean> {
    const inserted = await this.executor.query(
      `INSERT INTO ${this.tables.inbox} (message_id, received_at) VALUES (${
        placeholders(this.dialect, 2)
      })
       ON CONFLICT(message_id) DO NOTHING RETURNING message_id`,
      [messageId, now.toISOString()],
    );
    return inserted.length === 1;
  }
}

const KvInboxRecordSchema = Type.Object({
  messageId: Type.String(),
  receivedAt: Type.String(),
});

type KvInboxRecord = StaticDecode<typeof KvInboxRecordSchema>;

export const KvOutboxRecordSchema = Type.Object({
  id: Type.String(),
  kind: Type.String(),
  name: Type.String(),
  subject: Type.String(),
  payload: Type.String(),
  headers: Type.Record(Type.String(), Type.String()),
  state: Type.Union([
    Type.Literal("pending"),
    Type.Literal("dispatched"),
    Type.Literal("failed"),
    Type.Literal("claimed"),
  ]),
  attempts: Type.Number(),
  createdAt: Type.String(),
  updatedAt: Type.String(),
  nextAttemptAt: Type.Optional(Type.String()),
  lastError: Type.Optional(Type.String()),
  outcome: Type.Optional(Type.String()),
});

export type KvOutboxRecord = StaticDecode<typeof KvOutboxRecordSchema>;

/** Durable NATS KV outbox repository for services without SQL state. */
export class NatsKvOutboxRepository implements OutboxRepository {
  constructor(readonly kv: OutboxKvStore) {}

  async enqueue(rec: PreparedOutboxRecord): Promise<OutboxMessage> {
    const now = new Date().toISOString();
    const record: KvOutboxRecord = {
      id: rec.id,
      kind: rec.kind,
      name: rec.name,
      subject: rec.subject,
      payload: rec.payload,
      headers: { ...rec.headers },
      state: "pending",
      attempts: 0,
      createdAt: now,
      updatedAt: now,
    };
    const stored = await this.kv.create(record.id, record).take();
    if (!isErr(stored)) return kvRecordToOutboxMessage(record);
    if (!hasKvReason(stored.error, "exists")) throw stored.error;

    const existing = await this.kv.getEntry(record.id).take();
    if (isErr(existing)) throw existing.error;
    if (!existing) {
      throw new Error("outbox record missing after create conflict");
    }
    if (existing.value === undefined) {
      throw new Error("outbox record is deleted");
    }
    const message = kvRecordToOutboxMessage(existing.value);
    assertOutboxIdentity(message, rec);
    return message;
  }

  async get(id: string): Promise<OutboxMessage | undefined> {
    const loaded = await this.kv.getEntry(id).take();
    if (isErr(loaded)) {
      throw loaded.error;
    }
    if (!loaded || loaded.value === undefined) return undefined;
    return kvRecordToOutboxMessage(loaded.value);
  }

  async claimDue(limit: number, now: Date): Promise<OutboxMessage[]> {
    const keys = await this.kv.keys().take();
    if (isErr(keys)) throw keys.error;

    const dueAt = now.toISOString();
    const claimed: OutboxMessage[] = [];
    for await (const key of keys) {
      if (claimed.length >= limit) break;
      const loaded = await this.kv.getEntry(key).take();
      if (isErr(loaded)) {
        throw loaded.error;
      }
      if (!loaded) continue;
      const record = loaded.value;
      if (record === undefined) continue;
      if (record.state === "dispatched") {
        continue;
      }
      if (record.nextAttemptAt !== undefined && record.nextAttemptAt > dueAt) {
        continue;
      }

      const next: KvOutboxRecord = {
        ...record,
        state: "claimed",
        attempts: record.attempts + 1,
        updatedAt: dueAt,
        nextAttemptAt: new Date(now.getTime() + outboxClaimMs).toISOString(),
      };
      const stored = await this.kv.replace(key, loaded.revision, next).take();
      if (isErr(stored)) {
        if (hasKvReason(stored.error, "revision mismatch")) continue;
        throw stored.error;
      }
      claimed.push(kvRecordToOutboxMessage(next));
    }
    return claimed;
  }

  async markDispatched(
    claim: OutboxMessage,
    now: Date,
    outcome?: unknown,
  ): Promise<boolean> {
    const loaded = await this.kv.getEntry(claim.id).take();
    if (isErr(loaded)) {
      throw loaded.error;
    }
    if (!loaded || loaded.value === undefined) return false;
    if (
      loaded.value.state !== "claimed" ||
      loaded.value.attempts !== claim.attempts
    ) return false;
    const outcomeStr = outcome !== undefined
      ? JSON.stringify(outcome)
      : undefined;
    const stored = await this.kv.replace(claim.id, loaded.revision, {
      ...loaded.value,
      state: "dispatched",
      updatedAt: now.toISOString(),
      nextAttemptAt: undefined,
      lastError: undefined,
      outcome: outcomeStr ?? loaded.value.outcome,
    }).take();
    if (isErr(stored)) {
      if (hasKvReason(stored.error, "revision mismatch")) return false;
      throw stored.error;
    }
    return true;
  }

  async markFailed(
    claim: OutboxMessage,
    failure: { error: string; nextAttemptAt: Date; now: Date },
  ): Promise<boolean> {
    const loaded = await this.kv.getEntry(claim.id).take();
    if (isErr(loaded)) {
      throw loaded.error;
    }
    if (!loaded || loaded.value === undefined) return false;
    const record = loaded.value;
    if (record.state !== "claimed" || record.attempts !== claim.attempts) {
      return false;
    }
    const stored = await this.kv.replace(claim.id, loaded.revision, {
      ...record,
      state: "failed",
      updatedAt: failure.now.toISOString(),
      nextAttemptAt: failure.nextAttemptAt.toISOString(),
      lastError: failure.error,
    }).take();
    if (isErr(stored)) {
      if (hasKvReason(stored.error, "revision mismatch")) return false;
      throw stored.error;
    }
    return true;
  }
}

/** Durable NATS KV inbox repository for event-id duplicate suppression. */
export class NatsKvInboxRepository implements InboxRepository {
  constructor(readonly kv: TypedKV<KvInboxRecord>) {}

  async record(messageId: string, now: Date = new Date()): Promise<boolean> {
    // Durable NATS KV dedupe is useful for event handlers without SQL state, but
    // it is not transactional with unrelated DB side effects.
    const record: KvInboxRecord = { messageId, receivedAt: now.toISOString() };
    const stored = await this.kv.create(messageId, record);
    const value = stored.take();
    if (isErr(value)) {
      const reason = value.error.toSerializable().context?.["reason"];
      if (reason === "exists") return false;
      throw value.error;
    }
    return true;
  }
}

/**
 * Coalesces outbox wakeups and drains due messages through `dispatchOutbox`.
 *
 * The dispatcher is process-local coordination only. Callers should invoke
 * `notify()` after committing outbox rows so dispatch does not observe rolled
 * back work.
 */
export class OutboxDispatcher {
  readonly #repository: OutboxRepository;
  readonly #runtime: OutboxDispatchRuntime;
  readonly #options: OutboxDispatcherOptions;
  #wakeTimer: ReturnType<typeof setTimeout> | undefined;
  #retryTimer: ReturnType<typeof setTimeout> | undefined;
  #idleTimer: ReturnType<typeof setTimeout> | undefined;
  #running = false;
  #pending = false;
  #stopped = false;

  /** Creates a dispatcher over an existing outbox repository and runtime. */
  constructor(
    repository: OutboxRepository,
    runtime: OutboxDispatchRuntime,
    options: OutboxDispatcherOptions = {},
  ) {
    this.#repository = repository;
    this.#runtime = runtime;
    this.#options = options;
    this.#scheduleIdleRetry();
  }

  /** Signals that outbox work may be available and schedules a drain soon. */
  notify(): void {
    if (this.#stopped) return;
    this.#pending = true;
    if (this.#running) return;
    this.#scheduleWakeup(this.#options.debounceMs ?? 0);
  }

  /** Cancels pending wakeups and prevents future dispatch work. */
  stop(): void {
    this.#stopped = true;
    this.#pending = false;
    this.#clearTimer("wake");
    this.#clearTimer("retry");
    this.#clearTimer("idle");
  }

  #scheduleWakeup(delayMs: number): void {
    if (this.#stopped) return;
    this.#clearTimer("wake");
    this.#wakeTimer = setTimeout(() => {
      this.#wakeTimer = undefined;
      void this.#run();
    }, delayMs);
  }

  #scheduleRetryWakeup(): void {
    if (this.#stopped) return;
    if (this.#retryTimer !== undefined) return;
    this.#retryTimer = setTimeout(() => {
      this.#retryTimer = undefined;
      if (this.#stopped) return;
      this.#pending = true;
      if (!this.#running) this.#scheduleWakeup(0);
    }, this.#retryDelayMs());
  }

  #scheduleIdleRetry(): void {
    if (this.#stopped || this.#options.idleRetryMs === undefined) return;
    this.#clearTimer("idle");
    this.#idleTimer = setTimeout(() => {
      this.#idleTimer = undefined;
      this.notify();
    }, this.#options.idleRetryMs);
  }

  async #run(): Promise<void> {
    if (this.#stopped || this.#running) return;
    this.#running = true;
    this.#clearTimer("idle");
    try {
      do {
        this.#pending = false;
        while (!this.#stopped) {
          const result = await this.#dispatchBatch();
          if (result.failed > 0) {
            this.#scheduleRetryWakeup();
          }
          if (result.dispatched === 0 && result.failed === 0) break;
        }
      } while (this.#pending && !this.#stopped);
    } finally {
      this.#running = false;
      if (this.#pending && !this.#stopped) {
        this.#scheduleWakeup(0);
      } else {
        this.#scheduleIdleRetry();
      }
    }
  }

  async #dispatchBatch(): Promise<OutboxDispatchResult> {
    try {
      return await dispatchOutbox(this.#repository, this.#runtime, {
        limit: this.#options.limit,
        retryDelayMs: this.#retryDelayMs(),
      });
    } catch (error) {
      recordTrellisError(error, {
        surface: "outbox",
        direction: "dispatcher",
        operation: "batch",
        phase: "dispatch",
      });
      this.#scheduleRetryWakeup();
      try {
        this.#options.onError?.(error);
      } catch {
        // Error callbacks must not break dispatcher recovery.
      }
      return { dispatched: 0, failed: 0 };
    }
  }

  #retryDelayMs(): number {
    return Math.max(1, this.#options.retryDelayMs ?? 1000);
  }

  #clearTimer(timer: "wake" | "retry" | "idle"): void {
    if (timer === "wake" && this.#wakeTimer !== undefined) {
      clearTimeout(this.#wakeTimer);
      this.#wakeTimer = undefined;
    }
    if (timer === "retry" && this.#retryTimer !== undefined) {
      clearTimeout(this.#retryTimer);
      this.#retryTimer = undefined;
    }
    if (timer === "idle" && this.#idleTimer !== undefined) {
      clearTimeout(this.#idleTimer);
      this.#idleTimer = undefined;
    }
  }
}

/** Dispatches due outbox messages through a dispatch runtime. */
export async function dispatchOutbox(
  repository: OutboxRepository,
  runtime: OutboxDispatchRuntime,
  options: { limit?: number; now?: Date; retryDelayMs?: number } = {},
): Promise<OutboxDispatchResult> {
  const now = options.now ?? new Date();
  const retryDelayMs = options.retryDelayMs ?? 1000;
  const messages = await repository.claimDue(options.limit ?? 25, now);
  let dispatched = 0;
  let failed = 0;
  for (const message of messages) {
    if (message.kind === "event.publish") {
      const result = await runtime.publishPreparedEvent(
        outboxMessageToPreparedEvent(message),
      );
      const value = result.take();
      if (isErr(value)) {
        const failedAt = options.now ?? new Date();
        recordTrellisError(value.error, {
          surface: "outbox",
          direction: "dispatcher",
          operation: message.name,
          phase: "publish",
          messagingSystem: "nats",
        });
        if (
          await repository.markFailed(message, {
            error: value.error.message,
            nextAttemptAt: new Date(failedAt.getTime() + retryDelayMs),
            now: failedAt,
          })
        ) failed += 1;
        continue;
      }
      if (await repository.markDispatched(message, now)) dispatched += 1;
    } else if (
      message.kind === "job.create" || message.kind === "job.submit"
    ) {
      if (runtime.dispatchJobSubmission) {
        const result = await runtime.dispatchJobSubmission(message);
        const value = result.take();
        if (isErr(value)) {
          const failedAt = options.now ?? new Date();
          recordTrellisError(value.error, {
            surface: "outbox",
            direction: "dispatcher",
            operation: message.name,
            phase: "job.dispatch",
          });
          if (
            await repository.markFailed(message, {
              error: value.error.message,
              nextAttemptAt: new Date(failedAt.getTime() + retryDelayMs),
              now: failedAt,
            })
          ) failed += 1;
          continue;
        }
        if (await repository.markDispatched(message, now, value)) {
          dispatched += 1;
        }
      } else {
        recordTrellisError(new Error("job dispatch not supported"), {
          surface: "outbox",
          direction: "dispatcher",
          operation: message.name,
          phase: "dispatch",
        });
        await repository.markDispatched(message, now, {
          error: "job dispatch not supported",
        });
      }
    } else {
      const error = `Unknown outbox record kind: ${message.kind}`;
      recordTrellisError(new Error(error), {
        surface: "outbox",
        direction: "dispatcher",
        operation: message.name,
        phase: "dispatch",
      });
      await repository.markDispatched(message, now, { error });
    }
  }
  return { dispatched, failed };
}

/** Rehydrates a persisted outbox event row into a prepared event. */
export function outboxMessageToPreparedEvent(
  message: OutboxMessage,
): PreparedTrellisEvent {
  if (message.kind !== "event.publish") {
    throw new Error(
      `Expected event.publish kind, got ${message.kind}`,
    );
  }
  const payload = JSON.parse(message.payload) as Record<string, unknown>;
  const header = eventHeaderFromMessage(message.headers);
  const descriptorIdentity = message.headers["Trellis-Event-Descriptor"] ??
    message.headers["trellis-event-descriptor"];
  if (!descriptorIdentity) {
    throw new Error("Outbox event descriptor identity is required");
  }
  return Object.freeze({
    event: message.name,
    descriptorIdentity,
    subject: message.subject,
    header: Object.freeze(header),
    payload: Object.freeze(payload),
    encodedPayload: message.payload,
    headers: Object.freeze({ ...message.headers }),
  });
}

/** Adapts a prepared Trellis event into a generic outbox record. */
export function preparedTrellisEventToOutboxRecord(
  event: PreparedTrellisEvent,
): PreparedOutboxRecord {
  return {
    id: event.header.id,
    kind: "event.publish",
    name: event.event,
    subject: event.subject,
    payload: event.encodedPayload,
    headers: {
      ...event.headers,
      "Nats-Msg-Id": event.header.id,
      "Trellis-Event-Time": event.header.time,
      "Trellis-Event-Descriptor": event.descriptorIdentity,
    },
  };
}

function eventHeaderFromMessage(
  headers: Record<string, string>,
): { id: string; time: string } {
  const id = headers["Nats-Msg-Id"] ?? headers["nats-msg-id"];
  const time = headers["Trellis-Event-Time"] ?? headers["trellis-event-time"];
  return {
    id: typeof id === "string" ? id : "",
    time: typeof time === "string" ? time : new Date(0).toISOString(),
  };
}

function assertOutboxIdentity(
  existing: OutboxMessage,
  candidate: PreparedOutboxRecord,
): void {
  if (
    existing.kind !== candidate.kind || existing.name !== candidate.name ||
    existing.subject !== candidate.subject ||
    existing.payload !== candidate.payload ||
    Object.keys(existing.headers).length !==
      Object.keys(candidate.headers).length ||
    Object.entries(existing.headers).some(([key, value]) =>
      candidate.headers[key] !== value
    )
  ) {
    throw new OutboxDuplicateIdentityError(candidate.id);
  }
}

function rowToOutboxMessage(row: SqlRow): OutboxMessage {
  return {
    id: stringField(row, "id"),
    kind: kindField(row, "kind"),
    name: stringField(row, "name"),
    subject: stringField(row, "subject"),
    payload: stringField(row, "payload"),
    headers: parseHeaders(row["headers"]),
    state: stateField(row, "state"),
    attempts: numberField(row, "attempts"),
    createdAt: stringField(row, "created_at"),
    updatedAt: stringField(row, "updated_at"),
    nextAttemptAt: optionalStringField(row, "next_attempt_at"),
    lastError: optionalStringField(row, "last_error"),
    outcome: parseOutcome(row["outcome"]),
  };
}

function kvRecordToOutboxMessage(record: KvOutboxRecord): OutboxMessage {
  return {
    id: record.id,
    kind: kindField(record, "kind"),
    name: record.name,
    subject: record.subject,
    payload: record.payload,
    headers: { ...record.headers },
    state: record.state,
    attempts: record.attempts,
    createdAt: record.createdAt,
    updatedAt: record.updatedAt,
    nextAttemptAt: record.nextAttemptAt,
    lastError: record.lastError,
    outcome: parseOutcome(record.outcome),
  };
}

function parseOutcome(value: unknown): unknown {
  if (value === undefined || value === null) return undefined;
  return typeof value === "string" ? JSON.parse(value) : value;
}

function placeholder(dialect: SqlDialect, index: number): string {
  return dialect === "postgres" ? `$${index}` : "?";
}

function placeholders(dialect: SqlDialect, count: number): string {
  return Array.from(
    { length: count },
    (_, index) => placeholder(dialect, index + 1),
  ).join(", ");
}

function hasKvReason(error: BaseError, reason: string): boolean {
  if (error.toSerializable().context?.["reason"] === reason) return true;
  if (reason !== "revision mismatch") return false;
  const message = error.cause instanceof Error
    ? error.cause.message.toLowerCase()
    : "";
  return message.includes("wrong last sequence") ||
    message.includes("wrong last revision") ||
    message.includes("revision mismatch") ||
    message.includes("sequence mismatch");
}

function parseHeaders(value: unknown): Record<string, string> {
  if (typeof value === "string") {
    const parsed = JSON.parse(value);
    return recordOfStrings(parsed);
  }
  return recordOfStrings(value);
}

function recordOfStrings(value: unknown): Record<string, string> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Expected SQL headers object");
  }
  const out: Record<string, string> = {};
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry !== "string") {
      throw new Error("Expected SQL header value string");
    }
    out[key] = entry;
  }
  return out;
}

function stringField(row: SqlRow, field: string): string {
  const value = row[field];
  if (typeof value !== "string") {
    throw new Error(`Expected SQL field ${field} to be a string`);
  }
  return value;
}

function optionalStringField(row: SqlRow, field: string): string | undefined {
  const value = row[field];
  if (value === null || value === undefined) return undefined;
  if (typeof value !== "string") {
    throw new Error(`Expected SQL field ${field} to be a string`);
  }
  return value;
}

function numberField(row: SqlRow, field: string): number {
  const value = row[field];
  if (typeof value !== "number") {
    throw new Error(`Expected SQL field ${field} to be a number`);
  }
  return value;
}

function kindField(row: SqlRow, field: string): OutboxRecordKind {
  const value = stringField(row, field);
  if (
    value === "event.publish" || value === "job.create" ||
    value === "job.submit"
  ) {
    return value;
  }
  throw new Error(`Expected SQL field ${field} to be an outbox record kind`);
}

function stateField(row: SqlRow, field: string): OutboxMessageState {
  const value = stringField(row, field);
  if (
    value === "pending" || value === "claimed" || value === "dispatched" ||
    value === "failed"
  ) {
    return value;
  }
  throw new Error(`Expected SQL field ${field} to be an outbox state`);
}
