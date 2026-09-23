/** Test-only NATS WebSocket response gate. Forwards real bytes; may hold one RPC reply. */

const MAX_LINE_BYTES = 64 * 1024;
const MAX_RECORDS = 64;
const MAX_RETAINED_BYTES = 8 * 1024 * 1024;
export type NatsOp =
  | "INFO"
  | "CONNECT"
  | "PUB"
  | "HPUB"
  | "SUB"
  | "UNSUB"
  | "MSG"
  | "HMSG"
  | "PING"
  | "PONG"
  | "+OK"
  | "-ERR"
  | "UNKNOWN";

export type ParsedNatsRecord = {
  op: NatsOp;
  raw: Uint8Array;
  subject?: string;
  reply?: string;
  sid?: string;
  payload?: Uint8Array;
  headers?: Uint8Array;
};

const CRLF = new Uint8Array([13, 10]);
const decoder = new TextDecoder();

function indexOfCrlf(bytes: Uint8Array, start = 0): number {
  for (let i = start; i + 1 < bytes.length; i++) {
    if (bytes[i] === 13 && bytes[i + 1] === 10) return i;
  }
  return -1;
}

function concat(chunks: Uint8Array[]): Uint8Array<ArrayBuffer> {
  const length = chunks.reduce((sum, chunk) => sum + chunk.length, 0);
  const out = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

function copyBytes(
  bytes: Uint8Array,
  start = 0,
  end = bytes.length,
): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(end - start);
  out.set(bytes.subarray(start, end));
  return out;
}

function parseOpLine(line: string): {
  op: NatsOp;
  tokens: string[];
} {
  const trimmed = line.replace(/\r$/, "");
  const tokens = trimmed.split(" ").filter((token) => token.length > 0);
  const rawOp = tokens[0] ?? "UNKNOWN";
  const known: NatsOp[] = [
    "INFO",
    "CONNECT",
    "PUB",
    "HPUB",
    "SUB",
    "UNSUB",
    "MSG",
    "HMSG",
    "PING",
    "PONG",
    "+OK",
    "-ERR",
  ];
  const op = (known as string[]).includes(rawOp) ? rawOp as NatsOp : "UNKNOWN";
  return { op, tokens };
}

/** Incremental NATS protocol splitter. Records may span WebSocket frames. */
export class NatsProtocolParser {
  #buffer: Uint8Array<ArrayBuffer> = new Uint8Array(0);

  push(chunk: Uint8Array): void {
    this.#buffer = concat([this.#buffer, copyBytes(chunk)]);
  }

  takeRecords(): ParsedNatsRecord[] {
    const records: ParsedNatsRecord[] = [];
    while (true) {
      const record = this.#takeOne();
      if (!record) break;
      records.push(record);
    }
    return records;
  }

  leftoverBytes(): number {
    return this.#buffer.length;
  }

  #takeOne(): ParsedNatsRecord | undefined {
    const lineEnd = indexOfCrlf(this.#buffer);
    if (lineEnd < 0) {
      if (this.#buffer.length > MAX_LINE_BYTES) {
        throw new Error("NATS protocol line exceeded 64 KiB");
      }
      return;
    }
    if (lineEnd > MAX_LINE_BYTES) {
      throw new Error("NATS protocol line exceeded 64 KiB");
    }
    const lineBytes = copyBytes(this.#buffer, 0, lineEnd);
    const line = decoder.decode(lineBytes);
    const { op, tokens } = parseOpLine(line);
    if (
      op === "PING" || op === "PONG" || op === "+OK" || op === "INFO" ||
      op === "CONNECT" || op === "SUB" || op === "UNSUB" || op === "-ERR" ||
      op === "UNKNOWN"
    ) {
      const end = lineEnd + 2;
      const raw = copyBytes(this.#buffer, 0, end);
      this.#buffer = copyBytes(this.#buffer, end);
      return { op, raw };
    }
    if (op === "PUB" || op === "MSG") {
      return this.#takeBody(op, tokens, lineEnd);
    }
    if (op === "HPUB" || op === "HMSG") {
      return this.#takeHeaders(op, tokens, lineEnd);
    }
    const end = lineEnd + 2;
    const raw = copyBytes(this.#buffer, 0, end);
    this.#buffer = copyBytes(this.#buffer, end);
    return { op, raw };
  }

  #takeBody(
    op: "PUB" | "MSG",
    tokens: string[],
    lineEnd: number,
  ): ParsedNatsRecord | undefined {
    return this.#finishSized(op, parseBodySized(op, tokens), lineEnd, false);
  }

  #takeHeaders(
    op: "HPUB" | "HMSG",
    tokens: string[],
    lineEnd: number,
  ): ParsedNatsRecord | undefined {
    return this.#finishSized(op, parseHeaderSized(op, tokens), lineEnd, true);
  }

  #finishSized(
    op: "PUB" | "HPUB" | "MSG" | "HMSG",
    parsed: {
      subject: string;
      reply?: string;
      sid?: string;
      headerBytes: number;
      totalBytes: number;
    },
    lineEnd: number,
    headers: boolean,
  ): ParsedNatsRecord | undefined {
    const bodyStart = lineEnd + 2;
    const needed = bodyStart + parsed.totalBytes + 2;
    if (this.#buffer.length < needed) return;
    if (
      this.#buffer[needed - 2] !== 13 ||
      this.#buffer[needed - 1] !== 10
    ) {
      throw new Error(`${op} payload is missing trailing CRLF`);
    }
    const raw = copyBytes(this.#buffer, 0, needed);
    const body = copyBytes(
      this.#buffer,
      bodyStart,
      bodyStart + parsed.totalBytes,
    );
    this.#buffer = copyBytes(this.#buffer, needed);
    const headerBytes = headers
      ? copyBytes(body, 0, parsed.headerBytes)
      : undefined;
    const payload = headers
      ? copyBytes(body, parsed.headerBytes, parsed.totalBytes)
      : body;
    return {
      op,
      raw,
      subject: parsed.subject,
      reply: parsed.reply,
      sid: parsed.sid,
      headers: headerBytes,
      payload,
    };
  }
}

function parseBodySized(
  op: "PUB" | "MSG",
  tokens: string[],
): {
  subject: string;
  reply?: string;
  sid?: string;
  headerBytes: number;
  totalBytes: number;
} {
  // PUB subject [reply] size
  // MSG subject sid [reply] size
  if (op === "PUB") {
    if (tokens.length === 3) {
      return {
        subject: tokens[1],
        headerBytes: 0,
        totalBytes: Number(tokens[2]),
      };
    }
    if (tokens.length === 4) {
      return {
        subject: tokens[1],
        reply: tokens[2],
        headerBytes: 0,
        totalBytes: Number(tokens[3]),
      };
    }
  } else {
    if (tokens.length === 4) {
      return {
        subject: tokens[1],
        sid: tokens[2],
        headerBytes: 0,
        totalBytes: Number(tokens[3]),
      };
    }
    if (tokens.length === 5) {
      return {
        subject: tokens[1],
        sid: tokens[2],
        reply: tokens[3],
        headerBytes: 0,
        totalBytes: Number(tokens[4]),
      };
    }
  }
  throw new Error(`invalid ${op} line`);
}

function parseHeaderSized(
  op: "HPUB" | "HMSG",
  tokens: string[],
): {
  subject: string;
  reply?: string;
  sid?: string;
  headerBytes: number;
  totalBytes: number;
} {
  // HPUB subject [reply] hdr_len total_len
  // HMSG subject sid [reply] hdr_len total_len
  if (op === "HPUB") {
    if (tokens.length === 4) {
      return {
        subject: tokens[1],
        headerBytes: Number(tokens[2]),
        totalBytes: Number(tokens[3]),
      };
    }
    if (tokens.length === 5) {
      return {
        subject: tokens[1],
        reply: tokens[2],
        headerBytes: Number(tokens[3]),
        totalBytes: Number(tokens[4]),
      };
    }
  } else {
    if (tokens.length === 5) {
      return {
        subject: tokens[1],
        sid: tokens[2],
        headerBytes: Number(tokens[3]),
        totalBytes: Number(tokens[4]),
      };
    }
    if (tokens.length === 6) {
      return {
        subject: tokens[1],
        sid: tokens[2],
        reply: tokens[3],
        headerBytes: Number(tokens[4]),
        totalBytes: Number(tokens[5]),
      };
    }
  }
  throw new Error(`invalid ${op} line`);
}

export type HeldRequest = {
  route: string;
  reply?: string;
  requestId?: string;
};

export type ArmPredicate = (record: ParsedNatsRecord) => boolean;

type ArmedHold = {
  route: string;
  predicate?: ArmPredicate;
  resolve: (held: HeldRequest) => void;
};

function requestIdFromHeaders(headers?: Uint8Array): string | undefined {
  if (!headers) return;
  const text = decoder.decode(headers);
  const match = text.match(/(?:^|\r\n)request-id:\s*(\S+)/i);
  return match?.[1];
}

function routeMatches(subject: string, route: string): boolean {
  return subject === route || subject.startsWith(`${route}.`);
}

/**
 * Holds selected finite RPC replies after the real server produces them.
 * Live control/data, PING/PONG, and unrelated subjects are forwarded.
 */
export class NatsResponseGate {
  #inbound = new NatsProtocolParser();
  #outbound = new NatsProtocolParser();
  #armed: ArmedHold[] = [];
  #armedPush: Array<(record: ParsedNatsRecord) => void> = [];
  #held = new Map<string, Uint8Array>();
  #heldMeta = new Map<string, HeldRequest>();
  #retainedBytes = 0;
  #requests: Array<{ route: string; reply?: string }> = [];
  #pendingWaiters: Array<() => void> = [];
  #disposed = false;

  armNextReply(route: string, predicate?: ArmPredicate): Promise<HeldRequest> {
    return new Promise((resolve) => {
      this.#armed.push({ route, predicate, resolve });
    });
  }

  /**
   * Resolves with the next server push that is not a correlated RPC reply.
   * Used to observe that a real live frame reached the browser before acting.
   */
  armNextPush(): Promise<ParsedNatsRecord> {
    return new Promise((resolve) => this.#armedPush.push(resolve));
  }

  waitHeld(): Promise<void> {
    if (this.#held.size > 0) return Promise.resolve();
    return new Promise((resolve) => this.#pendingWaiters.push(resolve));
  }

  get heldRequest(): HeldRequest | undefined {
    return this.#heldMeta.values().next().value;
  }

  requestsSince(mark: number, routeSet: Iterable<string>): number {
    const routes = new Set(routeSet);
    return this.#requests.slice(mark).filter((item) =>
      item.route !== undefined &&
      [...routes].some((route) => routeMatches(item.route, route))
    ).length;
  }

  requestMark(): number {
    return this.#requests.length;
  }

  /** Forward every currently held server record. */
  release(): Uint8Array[] {
    const out = [...this.#held.values()];
    this.#held.clear();
    this.#heldMeta.clear();
    this.#retainedBytes = 0;
    return out;
  }

  dispose(): void {
    this.#disposed = true;
    this.release();
    this.#armed = [];
    this.#armedPush = [];
  }

  /** Server-to-client records. Returns bytes that should be forwarded now. */
  ingestServer(chunk: Uint8Array): Uint8Array[] {
    if (this.#disposed) return [chunk];
    this.#inbound.push(chunk);
    const out: Uint8Array[] = [];
    for (const record of this.#inbound.takeRecords()) {
      const forwarded = this.#considerServerRecord(record);
      if (forwarded) out.push(record.raw);
    }
    return out;
  }

  /** Client-to-server records. Always forwarded; used to arm reply correlation. */
  ingestClient(chunk: Uint8Array): Uint8Array[] {
    if (this.#disposed) return [chunk];
    this.#outbound.push(chunk);
    const out: Uint8Array[] = [];
    for (const record of this.#outbound.takeRecords()) {
      if (record.op === "PUB" || record.op === "HPUB") {
        if (record.subject) {
          this.#requests.push({
            route: record.subject,
            reply: record.reply,
          });
        }
      }
      out.push(record.raw);
    }
    return out;
  }

  #considerServerRecord(record: ParsedNatsRecord): boolean {
    if (record.op !== "MSG" && record.op !== "HMSG") return true;
    const reply = record.subject;
    if (!reply) return true;
    if (![...this.#requests].some((item) => item.reply === reply)) {
      const push = this.#armedPush.shift();
      if (push) push(record);
      return true;
    }
    const matchIndex = this.#armed.findIndex((arm) => {
      const request = [...this.#requests].reverse().find((item) =>
        item.reply === reply && routeMatches(item.route, arm.route)
      );
      if (!request) return false;
      return arm.predicate ? arm.predicate(record) : true;
    });
    if (matchIndex < 0) return true;
    if (this.#held.size >= MAX_RECORDS) {
      throw new Error("NATS response gate retained too many records");
    }
    this.#retainedBytes += record.raw.length;
    if (this.#retainedBytes > MAX_RETAINED_BYTES) {
      throw new Error("NATS response gate retained too many bytes");
    }
    const [arm] = this.#armed.splice(matchIndex, 1);
    const request = [...this.#requests].reverse().find((item) =>
      item.reply === reply
    );
    const held: HeldRequest = {
      route: request?.route ?? arm.route,
      reply,
      requestId: requestIdFromHeaders(record.headers),
    };
    this.#held.set(reply, record.raw);
    this.#heldMeta.set(reply, held);
    arm.resolve(held);
    for (const waiter of this.#pendingWaiters) waiter();
    this.#pendingWaiters = [];
    return false;
  }

  takeHeldRaw(reply: string): Uint8Array | undefined {
    const raw = this.#held.get(reply);
    if (!raw) return;
    this.#held.delete(reply);
    this.#heldMeta.delete(reply);
    this.#retainedBytes = Math.max(0, this.#retainedBytes - raw.length);
    return raw;
  }
}

export function encodeNatsPub(
  subject: string,
  payload: Uint8Array,
  reply?: string,
): Uint8Array {
  const line = reply
    ? `PUB ${subject} ${reply} ${payload.length}\r\n`
    : `PUB ${subject} ${payload.length}\r\n`;
  return concat([new TextEncoder().encode(line), payload, CRLF]);
}

export function encodeNatsMsg(
  subject: string,
  sid: string,
  payload: Uint8Array,
  reply?: string,
): Uint8Array {
  const line = reply
    ? `MSG ${subject} ${sid} ${reply} ${payload.length}\r\n`
    : `MSG ${subject} ${sid} ${payload.length}\r\n`;
  return concat([new TextEncoder().encode(line), payload, CRLF]);
}

void CRLF;
