/**
 * Bounded NATS client-protocol frame parser for the test transport proxy.
 *
 * The proxy must stay byte-transparent while it observes and, for one readiness
 * barrier, withholds a frame. That requires framing the wire protocol exactly:
 * length-delimited `PUB`/`HPUB`/`MSG`/`HMSG` payloads are never parsed
 * line-by-line, and a partial frame stays buffered until it is complete.
 *
 * Only the client-protocol verbs the test transport path actually uses are
 * decoded; any other complete control line is surfaced as an `unknown` frame so
 * forwarding never depends on understanding it.
 */

/** One decoded NATS protocol frame together with its exact raw bytes. */
export type NatsFrame = {
  op: NatsOp;
  raw: Uint8Array;
};

/** Decoded NATS protocol verb. */
export type NatsOp =
  | { kind: "connect" }
  | { kind: "ping" }
  | { kind: "pong" }
  | { kind: "sub"; subject: string; queue?: string; sid: string }
  | { kind: "unsub"; sid: string }
  | {
    kind: "pub";
    subject: string;
    reply?: string;
    headers?: Record<string, string>;
  }
  | { kind: "msg"; subject: string; sid: string; reply?: string }
  | { kind: "info" }
  | { kind: "ok" }
  | { kind: "err"; message: string }
  | { kind: "unknown" };

const CR = 13;
const LF = 10;
const textDecoder = new TextDecoder();
const textEncoder = new TextEncoder();

function indexOfCrlf(buffer: Uint8Array, from: number): number {
  for (let index = from; index + 1 < buffer.length; index++) {
    if (buffer[index] === CR && buffer[index + 1] === LF) return index;
  }
  return -1;
}

/** Parse one NATS header block (`NATS/1.0` then `Key: Value` lines). */
function parseHeaders(block: Uint8Array): Record<string, string> {
  const headers: Record<string, string> = {};
  const text = textDecoder.decode(block);
  for (const line of text.split("\r\n")) {
    if (line.length === 0 || line.startsWith("NATS/")) continue;
    const separator = line.indexOf(":");
    if (separator < 0) continue;
    headers[line.slice(0, separator).trim().toLowerCase()] = line
      .slice(separator + 1)
      .trim();
  }
  return headers;
}

/**
 * Incremental NATS frame parser. Callers push raw bytes and receive every
 * complete frame; an incomplete tail is retained for the next push.
 */
export class NatsFrameParser {
  #buffer = new Uint8Array(0);

  /**
   * Append bytes and return every complete frame now available.
   *
   * @param chunk Raw bytes received from the peer.
   */
  push(chunk: Uint8Array): NatsFrame[] {
    if (chunk.length > 0) {
      const merged = new Uint8Array(this.#buffer.length + chunk.length);
      merged.set(this.#buffer);
      merged.set(chunk, this.#buffer.length);
      this.#buffer = merged;
    }
    const frames: NatsFrame[] = [];
    let offset = 0;
    for (;;) {
      const lineEnd = indexOfCrlf(this.#buffer, offset);
      if (lineEnd < 0) break;
      const line = textDecoder.decode(this.#buffer.subarray(offset, lineEnd));
      const firstSpace = line.indexOf(" ");
      const verb = (firstSpace < 0 ? line : line.slice(0, firstSpace))
        .toUpperCase();
      let bodyLength = 0;
      let headerLength = 0;
      let payload = false;
      switch (verb) {
        case "PUB": {
          const parts = line.split(" ");
          bodyLength = Number(parts[parts.length - 1]);
          payload = true;
          break;
        }
        case "HPUB": {
          const parts = line.split(" ");
          headerLength = Number(parts[parts.length - 2]);
          bodyLength = Number(parts[parts.length - 1]);
          payload = true;
          break;
        }
        case "MSG": {
          const parts = line.split(" ");
          bodyLength = Number(parts[parts.length - 1]);
          payload = true;
          break;
        }
        case "HMSG": {
          const parts = line.split(" ");
          headerLength = Number(parts[parts.length - 2]);
          bodyLength = Number(parts[parts.length - 1]);
          payload = true;
          break;
        }
        default:
          break;
      }
      const frameEnd = payload ? lineEnd + 2 + bodyLength + 2 : lineEnd + 2;
      if (!Number.isSafeInteger(bodyLength) || bodyLength < 0) break;
      if (this.#buffer.length < frameEnd) break;
      const raw = this.#buffer.slice(offset, frameEnd);
      const op = this.#decode(
        verb,
        line,
        this.#buffer.subarray(lineEnd + 2, frameEnd),
        headerLength,
        bodyLength,
      );
      if (op) frames.push({ op, raw });
      offset = frameEnd;
    }
    this.#buffer = this.#buffer.slice(offset);
    return frames;
  }

  #decode(
    verb: string,
    line: string,
    payload: Uint8Array,
    headerLength: number,
    bodyLength: number,
  ): NatsOp | undefined {
    switch (verb) {
      case "CONNECT":
        return { kind: "connect" };
      case "PING":
        return { kind: "ping" };
      case "PONG":
        return { kind: "pong" };
      case "INFO":
        return { kind: "info" };
      case "+OK":
        return { kind: "ok" };
      case "-ERR":
        return { kind: "err", message: line.slice(5).trim() };
      case "SUB": {
        const [, subject, second, third] = line.split(" ");
        return second !== undefined && third !== undefined
          ? { kind: "sub", subject: subject!, queue: second, sid: third }
          : { kind: "sub", subject: subject!, sid: second! };
      }
      case "UNSUB": {
        const [, sid] = line.split(" ");
        return { kind: "unsub", sid: sid! };
      }
      case "PUB": {
        const parts = line.split(" ");
        const [, subject, second, third] = parts;
        return {
          kind: "pub",
          subject: subject!,
          reply: third === undefined ? undefined : second,
        };
      }
      case "HPUB": {
        const parts = line.split(" ");
        return {
          kind: "pub",
          subject: parts[1]!,
          reply: parts.length === 5 ? parts[2] : undefined,
          headers: parseHeaders(payload.subarray(0, headerLength)),
        };
      }
      case "MSG": {
        const parts = line.split(" ");
        return {
          kind: "msg",
          subject: parts[1]!,
          sid: parts[2]!,
          reply: parts.length === 5 ? parts[3] : undefined,
        };
      }
      case "HMSG": {
        const parts = line.split(" ");
        void bodyLength;
        return {
          kind: "msg",
          subject: parts[1]!,
          sid: parts[2]!,
          reply: parts.length === 6 ? parts[3] : undefined,
        };
      }
      default:
        return { kind: "unknown" };
    }
  }
}

/** Encode raw bytes for small protocol control frames. */
export function natsBytes(text: string): Uint8Array {
  return textEncoder.encode(text);
}
