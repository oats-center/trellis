import {
  AckPolicy,
  DeliverPolicy,
  type DirectStreamAPI,
  jetstream,
  type JetStreamClient,
  jetstreamManager,
} from "@nats-io/jetstream";
import { createInbox, type NatsConnection } from "@nats-io/nats-core";

import type { AuthorizationRegistryBinding } from "./types.ts";

const REVOCATION_PREFIX = "revocation.";

/** Registry I/O counters observed since provider-cache start. */
export type AuthorizationRegistryIoCounters = {
  contextGets: number;
  watchStarts: number;
};

export type RegistryEntry = {
  value: Uint8Array;
  revision: number;
  operation: string;
};

/** One exact revocation-key update observed after subscription flush. */
export type RegistryWatchEntry =
  | { operation: "put"; key: string; value: Uint8Array; revision: number }
  | { operation: "delete"; key: string; revision: number }
  | { operation: "initialized" };

class RegistryWatchQueue implements AsyncIterator<RegistryWatchEntry> {
  readonly #values: RegistryWatchEntry[] = [];
  #waiting?: {
    resolve: (result: IteratorResult<RegistryWatchEntry>) => void;
    reject: (error: unknown) => void;
  };
  #closed = false;
  #error?: Error;

  push(value: RegistryWatchEntry): void {
    if (this.#closed) return;
    if (this.#waiting) {
      const waiting = this.#waiting;
      this.#waiting = undefined;
      waiting.resolve({ done: false, value });
    } else if (this.#values.length < 2) {
      this.#values.push(value);
    } else {
      throw new Error("authorization revocation watch queue overflow");
    }
  }

  fail(error: Error): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#error = error;
    this.#values.length = 0;
    this.#waiting?.reject(error);
    this.#waiting = undefined;
  }

  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#values.length = 0;
    this.#waiting?.resolve({ done: true, value: undefined });
    this.#waiting = undefined;
  }

  next(): Promise<IteratorResult<RegistryWatchEntry>> {
    if (this.#error) return Promise.reject(this.#error);
    if (this.#closed) {
      return Promise.resolve({ done: true, value: undefined });
    }
    const value = this.#values.shift();
    if (value) return Promise.resolve({ done: false, value });
    return new Promise((resolve, reject) =>
      this.#waiting = { resolve, reject }
    );
  }
}

/** Connected NATS KV reader for authorization evidence. */
export class AuthorizationRegistryReader {
  readonly #nats: NatsConnection;
  readonly #jetstream: JetStreamClient;
  readonly #direct: DirectStreamAPI;
  readonly #binding: AuthorizationRegistryBinding;
  readonly #inboxPrefix: string;
  #contextGets = 0;
  #watchStarts = 0;

  private constructor(
    nats: NatsConnection,
    jetstreamClient: JetStreamClient,
    direct: DirectStreamAPI,
    binding: AuthorizationRegistryBinding,
    inboxPrefix: string,
  ) {
    this.#nats = nats;
    this.#jetstream = jetstreamClient;
    this.#direct = direct;
    this.#binding = binding;
    this.#inboxPrefix = inboxPrefix;
  }

  /** Open the exact registry buckets from bootstrap-owned internal metadata. */
  static async open(
    nats: NatsConnection,
    binding: AuthorizationRegistryBinding,
    inboxPrefix: string,
  ): Promise<AuthorizationRegistryReader> {
    validateBinding(binding);
    const manager = await jetstreamManager(nats);
    return new AuthorizationRegistryReader(
      nats,
      jetstream(nats),
      manager.direct,
      binding,
      inboxPrefix,
    );
  }

  /** Return a copy of internal registry counters. */
  ioCounters(): AuthorizationRegistryIoCounters {
    return {
      contextGets: this.#contextGets,
      watchStarts: this.#watchStarts,
    };
  }

  /** Read one immutable context by its exact digest key. */
  async getContext(digest: string): Promise<RegistryEntry | null> {
    assertRegistryKey(digest, "authorization context digest");
    this.#contextGets += 1;
    return await this.#putOrNull(
      this.#binding.contextBucket,
      digest,
    );
  }

  /** Subscribe only to the active context's revocation key. */
  async watchRevocation(contextDigest: string): Promise<{
    iterator: AsyncIterator<RegistryWatchEntry>;
    close: () => Promise<void>;
  }> {
    assertRegistryKey(contextDigest, "authorization context digest");
    this.#watchStarts += 1;
    const key = `${REVOCATION_PREFIX}${contextDigest}`;
    const stream = `KV_${this.#binding.contextBucket}`;
    const filterSubject = `$KV.${this.#binding.contextBucket}.${key}`;
    const deliverSubject = createInbox(this.#inboxPrefix);
    const name = `TrellisAuth${deliverSubject.split(".").at(-1) ?? ""}`;
    const manager = await this.#jetstream.jetstreamManager();
    await manager.consumers.add(stream, {
      ack_policy: AckPolicy.None,
      deliver_policy: DeliverPolicy.LastPerSubject,
      deliver_subject: deliverSubject,
      filter_subject: filterSubject,
      flow_control: true,
      idle_heartbeat: 5_000_000_000,
      inactive_threshold: 10_000_000_000,
      mem_storage: true,
      name,
      num_replicas: 1,
    });
    const consumer = await this.#jetstream.consumers.getPushConsumer(
      stream,
      name,
    );
    const queue = new RegistryWatchQueue();
    let receivedConsumerSequence = 0;
    let lastStreamRevision = 0;
    let initialBoundary: number | undefined;
    let initialized = false;
    let stopped = false;
    const initialize = () => {
      if (
        !initialized && initialBoundary !== undefined &&
        receivedConsumerSequence >= initialBoundary
      ) {
        initialized = true;
        queue.push({ operation: "initialized" });
      }
    };
    let stop = (error?: Error) => {
      stopped = true;
      if (error) queue.fail(error);
      else queue.close();
    };
    const messages = await consumer.consume({
      callback: (message) => {
        try {
          const current = message.info.deliverySequence;
          const streamRevision = message.info.streamSequence;
          if (
            message.subject !== filterSubject ||
            current !== receivedConsumerSequence + 1 ||
            streamRevision <= lastStreamRevision
          ) {
            throw new Error("authorization revocation watch sequence gap");
          }
          receivedConsumerSequence = current;
          lastStreamRevision = streamRevision;
          const operation = message.headers?.has("KV-Operation")
            ? message.headers.get("KV-Operation")
            : "PUT";
          if (operation === "DEL" || operation === "PURGE") {
            queue.push({ operation: "delete", key, revision: streamRevision });
          } else if (operation === "PUT") {
            queue.push({
              operation: "put",
              key,
              value: message.data,
              revision: streamRevision,
            });
          } else {
            throw new Error("authorization revocation operation is invalid");
          }
          initialize();
        } catch (error) {
          stop(error instanceof Error ? error : new Error(String(error)));
        }
      },
    });
    stop = (error?: Error) => {
      if (stopped) return;
      stopped = true;
      if (error) queue.fail(error);
      else queue.close();
      messages.stop(error);
    };
    const status = messages.status();
    const statusTask = (async () => {
      try {
        for await (const event of status) {
          if (
            event.type === "heartbeat" &&
            event.lastConsumerSequence > receivedConsumerSequence
          ) {
            stop(new Error("authorization revocation watch sequence gap"));
            return;
          }
          if (
            event.type === "heartbeats_missed" ||
            event.type === "consumer_deleted" ||
            event.type === "stream_not_found" ||
            event.type === "consumer_not_found" ||
            event.type === "no_responders" ||
            event.type === "reset" ||
            event.type === "ordered_consumer_recreated"
          ) {
            stop(
              new Error(
                `authorization revocation watch lost coverage: ${event.type}`,
              ),
            );
            return;
          }
        }
      } catch (error) {
        stop(error instanceof Error ? error : new Error(String(error)));
      } finally {
        if (!stopped) {
          stop(new Error("authorization revocation watch status ended"));
        }
      }
    })();
    const settled = async () => {
      await messages.closed();
      await statusTask;
    };
    try {
      await this.#nats.flush();
      const info = await consumer.info();
      initialBoundary = info.delivered.consumer_seq + info.num_pending;
      initialize();
    } catch (error) {
      stop(error instanceof Error ? error : new Error(String(error)));
      await settled();
      throw error;
    }
    void messages.closed().then((error) => {
      if (!stopped) {
        stop(
          error instanceof Error
            ? error
            : new Error("authorization revocation watch ended"),
        );
      }
    });
    return {
      iterator: {
        next: () => queue.next(),
        return: async () => {
          stop();
          await settled();
          return { done: true, value: undefined };
        },
      },
      close: async () => {
        stop();
        await settled();
      },
    };
  }

  async #putOrNull(bucket: string, key: string): Promise<RegistryEntry | null> {
    let entry: Awaited<ReturnType<DirectStreamAPI["getMessage"]>>;
    try {
      entry = await this.#direct.getMessage(`KV_${bucket}`, {
        last_by_subj: `$KV.${bucket}.${key}`,
      });
    } catch (error) {
      if (
        typeof error === "object" && error !== null && "code" in error &&
        error.code === 404
      ) return null;
      throw error;
    }
    if (!entry) return null;
    const operation = entry.header?.get("KV-Operation") || "PUT";
    if (operation !== "PUT") {
      throw new Error("authorization registry evidence disappeared");
    }
    return { value: entry.data, revision: entry.seq, operation };
  }
}

function validateBinding(binding: AuthorizationRegistryBinding): void {
  const entries = Object.entries(binding);
  if (
    entries.length !== 1 || !("contextBucket" in binding)
  ) {
    throw new Error("authorization registry binding is invalid");
  }
  for (const [name, value] of entries) {
    if (typeof value !== "string" || !value.trim()) {
      throw new Error(`authorization registry binding ${name} is empty`);
    }
  }
}

function assertRegistryKey(value: string, name: string): void {
  if (!isRegistryKey(value)) throw new Error(`${name} is invalid`);
}

function isRegistryKey(value: string): boolean {
  return value.length === 43 && /^[A-Za-z0-9_-]+$/.test(value);
}
