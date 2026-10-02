import {
  AckPolicy,
  type ConsumerMessages,
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
    // Deliver already-recorded evidence before reporting loss of coverage.
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
    const value = this.#values.shift();
    if (value) return Promise.resolve({ done: false, value });
    if (this.#error) return Promise.reject(this.#error);
    if (this.#closed) {
      return Promise.resolve({ done: true, value: undefined });
    }
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

  /**
   * Open the exact registry buckets from bootstrap-owned internal metadata.
   *
   * The JetStream manager discovery is a plain request that owns no consumer or
   * subscription. When `signal` is supplied, an already-aborted signal rejects
   * before any registry I/O, and an abort during discovery rejects the open
   * promise without waiting out the ordinary NATS request timeout; the abort
   * listener is removed once either side settles. A late settlement of a
   * discovery whose caller already aborted is consumed silently.
   */
  static async open(
    nats: NatsConnection,
    binding: AuthorizationRegistryBinding,
    inboxPrefix: string,
    signal?: AbortSignal,
  ): Promise<AuthorizationRegistryReader> {
    validateBinding(binding);
    if (signal?.aborted) throw abortReason(signal);
    const managerPromise = jetstreamManager(nats);
    // This discovery owns no consumer or subscription. If the caller aborts and
    // this race rejects first, keep the pending request's eventual settlement
    // from surfacing as an unhandled rejection.
    void managerPromise.catch(() => undefined);
    let manager: Awaited<ReturnType<typeof jetstreamManager>>;
    if (signal === undefined) {
      manager = await managerPromise;
    } else {
      let onAbort: (() => void) | undefined;
      const aborted = new Promise<never>((_, reject) => {
        onAbort = () => reject(abortReason(signal));
        signal.addEventListener("abort", onAbort, { once: true });
      });
      try {
        manager = await Promise.race([managerPromise, aborted]);
      } finally {
        if (onAbort) signal.removeEventListener("abort", onAbort);
      }
    }
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

  /**
   * Subscribe only to the active context's revocation key.
   *
   * The watch is a live ordered view of exactly that key. When `signal` aborts,
   * a still-initializing watch rejects with the signal's reason (a standard
   * `AbortError` by default) and an initialized watch rejects its next read;
   * both paths stop the owned push subscription and status watcher so no local
   * resource outlives the watch. The ephemeral JetStream consumer the watch
   * creates is not deleted here: once its subscription stops it has no local
   * owner, and the broker reaps it after `inactive_threshold`. An abort before
   * the call performs no registry I/O.
   */
  async watchRevocation(
    contextDigest: string,
    signal?: AbortSignal,
  ): Promise<{
    iterator: AsyncIterator<RegistryWatchEntry>;
    close: () => Promise<void>;
    /** Synchronous local coverage fence, including failures not yet consumed. @internal */
    usable: () => boolean;
  }> {
    assertRegistryKey(contextDigest, "authorization context digest");
    if (signal?.aborted) throw abortReason(signal);
    this.#watchStarts += 1;
    const key = `${REVOCATION_PREFIX}${contextDigest}`;
    const stream = `KV_${this.#binding.contextBucket}`;
    const filterSubject = `$KV.${this.#binding.contextBucket}.${key}`;
    const deliverSubject = createInbox(this.#inboxPrefix);
    const name = `TrellisAuth${deliverSubject.split(".").at(-1) ?? ""}`;
    const queue = new RegistryWatchQueue();
    let receivedConsumerSequence = 0;
    let lastStreamRevision = 0;
    let initialBoundary: number | undefined;
    let initialized = false;
    let stopped = false;
    let live = false;
    let disposed = false;
    let messages: ConsumerMessages | undefined;
    let statusTask: Promise<void> | undefined;
    let terminalError: Error | undefined;

    const stopMessages = (error?: Error): void => {
      try {
        messages?.stop(error);
      } catch {
        // A local subscription teardown problem must not mask the real cause.
      }
    };
    // Idempotent local teardown: the first caller stops the owned push
    // subscription and status watcher, and a later caller (including a late
    // abort) is a no-op. Remote consumer deletion is deliberately not
    // attempted; the broker reaps the consumer after `inactive_threshold`.
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      stopMessages();
    };

    let rejectAbort: ((error: Error) => void) | undefined;
    const abortPromise = new Promise<never>((_, reject) => {
      rejectAbort = reject;
    });
    // Setup races consume this rejection; keep any later one handled too.
    void abortPromise.catch(() => undefined);

    const initialize = (): void => {
      if (
        !initialized && initialBoundary !== undefined &&
        receivedConsumerSequence >= initialBoundary
      ) {
        initialized = true;
        queue.push({ operation: "initialized" });
      }
    };
    // One stable stop: it records the terminal failure, settles the queue, and
    // stops whatever push subscription is owned. A callback or status watcher
    // can request a stop while the subscription is still being created, before
    // there is an owner to stop; setup observes `stopped` once it owns the
    // subscription and honours that earlier stop with this recorded cause.
    const stop = (error?: Error): void => {
      if (stopped) return;
      stopped = true;
      terminalError = error;
      if (error) queue.fail(error);
      else queue.close();
      stopMessages(error);
    };
    const settled = async (): Promise<void> => {
      if (messages) await messages.closed();
      if (statusTask) await statusTask;
    };
    const handleAbort = (): void => {
      const reason = abortReason(signal);
      if (!live) {
        rejectAbort?.(reason);
        return;
      }
      stop(reason);
      void settled().then(() => dispose());
    };
    const removeAbortListener = (): void => {
      signal?.removeEventListener("abort", handleAbort);
    };
    const raceAbort = <T>(operation: Promise<T>): Promise<T> =>
      signal === undefined
        ? operation
        : Promise.race([operation, abortPromise]);
    if (signal) {
      if (signal.aborted) handleAbort();
      else signal.addEventListener("abort", handleAbort, { once: true });
    }

    const setup = async (): Promise<ConsumerMessages> => {
      const manager = await raceAbort(this.#jetstream.jetstreamManager());
      const addPromise = manager.consumers.add(stream, {
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
      // The consumer may be created even when the abort wins this race. It then
      // has no local owner and the broker reaps it after `inactive_threshold`,
      // so only keep its eventual rejection from surfacing as unhandled.
      try {
        await raceAbort(addPromise);
      } catch (error) {
        void addPromise.catch(() => undefined);
        throw error;
      }
      const consumer = await raceAbort(
        this.#jetstream.consumers.getPushConsumer(stream, name),
      );
      const consumePromise = consumer.consume({
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
              queue.push({
                operation: "delete",
                key,
                revision: streamRevision,
              });
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
      // A subscription created after the abort won the race is unreachable by
      // the caller, so the late resolution must still stop it.
      const opened = await raceAbort(consumePromise).catch((error) => {
        void consumePromise.then((subscription) => {
          try {
            subscription.stop();
          } catch {
            // Late cleanup is best effort.
          }
        }, () => undefined);
        throw error;
      });
      messages = opened;
      // An early sequence or error may have stopped the watch before the push
      // subscription existed to stop. Now that it is owned, honour that stop
      // and fail setup with its recorded cause instead of returning a watch
      // whose subscription is still live.
      if (stopped) {
        stopMessages(terminalError);
        throw terminalError ??
          new Error("authorization revocation watch stopped during setup");
      }
      const status = opened.status();
      statusTask = (async () => {
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
      await raceAbort(this.#nats.flush());
      const info = await raceAbort(consumer.info());
      initialBoundary = info.delivered.consumer_seq + info.num_pending;
      initialize();
      return opened;
    };

    let opened: ConsumerMessages;
    try {
      opened = await setup();
    } catch (error) {
      removeAbortListener();
      stop(error instanceof Error ? error : new Error(String(error)));
      await settled();
      dispose();
      throw error;
    }
    live = true;
    // Close the narrow window where the abort fired as setup was resolving:
    // the listener saw a not-yet-live watch, so shut the live watch down here.
    if (signal?.aborted) handleAbort();
    void opened.closed().then((error) => {
      if (!stopped) {
        stop(
          error instanceof Error
            ? error
            : new Error("authorization revocation watch ended"),
        );
      }
    });
    return {
      usable: () => initialized && !stopped && !disposed && !signal?.aborted,
      iterator: {
        next: () => queue.next(),
        return: async () => {
          stop();
          await settled();
          dispose();
          removeAbortListener();
          return { done: true, value: undefined };
        },
      },
      close: async () => {
        stop();
        await settled();
        dispose();
        removeAbortListener();
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

function abortReason(signal: AbortSignal | undefined): Error {
  const reason: unknown = signal?.reason;
  if (reason instanceof Error) return reason;
  if (reason === undefined) {
    return new DOMException(
      "authorization revocation watch aborted",
      "AbortError",
    );
  }
  return new Error(String(reason));
}

function assertRegistryKey(value: string, name: string): void {
  if (!isRegistryKey(value)) throw new Error(`${name} is invalid`);
}

function isRegistryKey(value: string): boolean {
  return value.length === 43 && /^[A-Za-z0-9_-]+$/.test(value);
}
