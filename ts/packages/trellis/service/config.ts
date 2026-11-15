import { renderUnicodeCompact } from "uqr";

import type { ConfigFieldDescriptor } from "../generated_support.ts";
import { API as AUTH_API } from "../internal_sdk/generated/apis/auth/mod.js";
import type {
  AuthAppConfigResponse,
  AuthOwnConfigResponse,
} from "../internal_sdk/generated/types/index.js";
import type { LoggerLike } from "../globals.ts";
import type { TrellisTransportProvider } from "../transport/generations.ts";

/** One live change to a participant's settings. */
export type ConfigChange<TValues> = {
  /** Values before the change. */
  readonly previous: TValues;
  /** Values now in effect. */
  readonly current: TValues;
  /** Fields whose value changed. Restart-only fields never appear here. */
  readonly changed: readonly (keyof TValues & string)[];
};

/** Revisions behind the values in effect. */
export type ConfigRevision = {
  /** A service's or device's deployment revision. */
  readonly deployment?: number;
  /** An instance's override revision, when it has overrides. */
  readonly instance?: number;
  /** An app's revision. */
  readonly app?: number;
};

/**
 * Settings Trellis keeps for this service, device, or app, declared with
 * `config` in its contract and set by administrators through the CLI or Console.
 */
export interface ParticipantConfig<TValues> {
  /**
   * Values in effect. A restart-only field keeps the value loaded when the
   * participant connected until it connects again.
   */
  readonly current: TValues;
  /** The revisions behind `current`. */
  readonly revision: ConfigRevision;
  /**
   * Run `listener` after an administrator changes a field that applies live.
   * Returns a function that removes the listener.
   */
  onChange(listener: (change: ConfigChange<TValues>) => void): () => void;
}

/** Fields a participant declares, keyed by name. */
export type ConfigFields = Readonly<Record<string, ConfigFieldDescriptor>>;

/** One reading of the participant's settings from Auth. */
export type ConfigReading = {
  readonly values: Readonly<Record<string, unknown>>;
  readonly revision: ConfigRevision;
};

/** Reads the participant's own settings from Auth. */
export type ConfigReader = (atConnect: boolean) => Promise<ConfigReading>;

/** A service's or device's own reading, from `Config.Own.Get`. */
export function ownReading(response: AuthOwnConfigResponse): ConfigReading {
  return {
    values: response.values,
    revision: {
      ...(response.deploymentRevision === undefined
        ? {}
        : { deployment: response.deploymentRevision }),
      ...(response.instanceRevision === undefined
        ? {}
        : { instance: response.instanceRevision }),
    },
  };
}

/** Decoded setting values a generated participant declares. */
export type ParticipantConfigOf<TContract> = TContract extends {
  readonly __runtimeTypes?: { config: infer TConfig };
}
  ? TConfig
  : Record<string, never>;

/** Decoded setting values a generated app request declares. */
export type AppConfigOf<TRequest> = TRequest extends {
  readonly config: infer TFields;
}
  ? {
      readonly [K in keyof TFields]: TFields[K] extends {
        readonly codec: { decode(value: unknown): infer TValue };
      }
        ? TValue
        : never;
    }
  : Record<string, never>;

/**
 * Read an app's settings at connect and follow changes to its client's
 * settings until `closed` settles. App settings always have defaults, so an
 * app never waits.
 * @internal Used by the browser and native app connect paths.
 */
export async function connectAppConfig<TValues>(args: {
  fields: ConfigFields | undefined;
  clientId: string;
  read: () => Promise<AuthAppConfigResponse>;
  transport: TrellisTransportProvider;
  closed: Promise<unknown>;
  log: LoggerLike;
  timeoutMs: number;
}): Promise<ParticipantConfig<TValues>> {
  const fields = args.fields ?? {};
  const read: ConfigReader = async () => {
    const response = await args.read();
    return {
      values: response.values,
      revision:
        response.revision === undefined ? {} : { app: response.revision },
    };
  };
  // An app without settings reads nothing: it may not hold `config.readApp`.
  if (Object.keys(fields).length === 0)
    return new ConfigHandle<TValues>(
      fields,
      { values: {}, revision: {} },
      args.log,
    );
  const handle = new ConfigHandle<TValues>(fields, await read(true), args.log);
  handle.watch({
    transport: args.transport,
    subject: AUTH_API.actions["event:Config.Changed"].subject,
    matches: (event) => event.clientId === args.clientId,
    read,
    timeoutMs: args.timeoutMs,
  });
  void args.closed.finally(() => handle.stop());
  return handle;
}

/** The `config` of a participant that declares no settings. */
export const NO_CONFIG: ParticipantConfig<Record<string, unknown>> = {
  current: {},
  revision: {},
  onChange: () => () => {},
};

/**
 * Live handle over decoded settings.
 * @internal Constructed by the service and device connect paths.
 */
export class ConfigHandle<TValues> implements ParticipantConfig<TValues> {
  readonly #fields: ConfigFields;
  readonly #log: LoggerLike;
  // Decoded values by field name; `current` presents them as `TValues`.
  #current: Record<string, unknown>;
  #revision: ConfigRevision;
  readonly #listeners = new Set<(change: ConfigChange<TValues>) => void>();
  readonly #stop = new AbortController();
  #refreshing: Promise<void> = Promise.resolve();

  constructor(fields: ConfigFields, loaded: ConfigReading, log: LoggerLike) {
    this.#fields = fields;
    this.#log = log;
    this.#current = decodeValues(fields, loaded.values);
    this.#revision = loaded.revision;
  }

  get current(): TValues {
    return this.#current as TValues;
  }

  get revision(): ConfigRevision {
    return this.#revision;
  }

  onChange(listener: (change: ConfigChange<TValues>) => void): () => void {
    this.#listeners.add(listener);
    return () => {
      this.#listeners.delete(listener);
    };
  }

  /**
   * Take a newer reading. Restart-only fields keep their connect-time value,
   * and a field whose value was removed keeps its last one until reconnect,
   * when the participant waits for it again.
   */
  apply(next: ConfigReading): void {
    const decoded = decodeValues(this.#fields, next.values);
    const previous = this.#current;
    const current: Record<string, unknown> = { ...previous };
    const changed: (keyof TValues & string)[] = [];
    for (const [name, field] of Object.entries(this.#fields)) {
      if (field.restart || !(name in decoded)) continue;
      if (!sameValue(decoded[name], previous[name])) {
        current[name] = decoded[name];
        changed.push(name as keyof TValues & string);
      }
    }
    this.#revision = next.revision;
    if (changed.length === 0) return;
    this.#current = current;
    const change = {
      previous: previous as TValues,
      current: current as TValues,
      changed,
    };
    for (const listener of this.#listeners) {
      try {
        listener(change);
      } catch (error) {
        this.#log.error({ error }, "A config change listener threw");
      }
    }
  }

  /**
   * Follow `Config.Changed` events `matches` accepts until stopped.
   * Each event only prompts a fresh authenticated read, so its payload is
   * never trusted for values.
   * @internal
   */
  watch(args: {
    transport: TrellisTransportProvider;
    subject: string;
    matches: (event: Readonly<Record<string, unknown>>) => boolean;
    read: ConfigReader;
    timeoutMs: number;
  }): void {
    const signal = this.#stop.signal;
    const refresh = (): Promise<void> => {
      this.#refreshing = this.#refreshing.then(async () => {
        if (signal.aborted) return;
        try {
          this.apply(await args.read(false));
        } catch (error) {
          if (!signal.aborted) {
            this.#log.warn({ error }, "Could not read changed settings");
          }
        }
      });
      return this.#refreshing;
    };
    void (async () => {
      while (!signal.aborted) {
        let lease:
          | Awaited<ReturnType<TrellisTransportProvider["acquireFor"]>>
          | undefined;
        let unwatch = (): void => {};
        try {
          lease = await args.transport.acquireFor(
            { subscribe: [args.subject] },
            { signal, deadlineMs: Date.now() + args.timeoutMs },
          );
          const subscription = lease.nc.subscribe(args.subject);
          const close = () => subscription.unsubscribe();
          signal.addEventListener("abort", close, { once: true });
          // Changes made while no subscription existed are read now.
          unwatch = lease.watchStatus((event) => {
            if (
              typeof event === "object" &&
              event !== null &&
              (event as { type?: unknown }).type === "reconnect"
            ) {
              void refresh();
            }
          });
          await lease.nc.flush();
          await refresh();
          for await (const message of subscription) {
            let event: unknown;
            try {
              event = message.json();
            } catch {
              continue;
            }
            if (
              typeof event === "object" &&
              event !== null &&
              args.matches(event as Record<string, unknown>)
            )
              await refresh();
          }
          signal.removeEventListener("abort", close);
        } catch (error) {
          if (signal.aborted) break;
          this.#log.warn({ error }, "Settings watch interrupted; retrying");
          await new Promise<void>((resolve) => {
            const timer = setTimeout(resolve, 1_000);
            signal.addEventListener(
              "abort",
              () => {
                clearTimeout(timer);
                resolve();
              },
              { once: true },
            );
          });
        } finally {
          unwatch();
          lease?.release();
        }
      }
    })();
  }

  /** Stop following changes. @internal */
  stop(): void {
    this.#stop.abort();
  }
}

/**
 * Read the participant's settings at connect. While a required value is
 * missing, print where an administrator sets it and wait; never start with a
 * partial configuration.
 * @internal
 */
export async function loadConfig<
  TValues extends Record<string, unknown>,
>(args: {
  participantId: string;
  fields: ConfigFields;
  read: (atConnect: boolean) => Promise<AuthOwnConfigResponse>;
  log: LoggerLike;
  signal?: AbortSignal;
}): Promise<ConfigHandle<TValues>> {
  let delay = 1_000;
  let shown = "";
  for (;;) {
    const loaded = await args.read(true);
    if (loaded.missing.length === 0) {
      return new ConfigHandle<TValues>(
        args.fields,
        ownReading(loaded),
        args.log,
      );
    }
    const missing = loaded.missing.join(", ");
    if (shown !== missing) {
      shown = missing;
      args.log.warn(
        {
          participant: args.participantId,
          missing: loaded.missing,
          configUrl: loaded.configUrl,
        },
        "Waiting for an administrator to set required settings",
      );
      console.error(
        loaded.configUrl === undefined
          ? `\n${args.participantId} needs these settings before it starts: ${missing}`
          : `\n${args.participantId} needs these settings before it starts: ${missing}\nSet them at ${loaded.configUrl}\nor scan:\n${renderUnicodeCompact(loaded.configUrl)}`,
      );
    }
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(resolve, delay);
      args.signal?.addEventListener(
        "abort",
        () => {
          clearTimeout(timer);
          reject(args.signal?.reason);
        },
        { once: true },
      );
    });
    delay = Math.min(delay * 2, 15_000);
  }
}

function decodeValues(
  fields: ConfigFields,
  values: Readonly<Record<string, unknown>>,
): Record<string, unknown> {
  const decoded: Record<string, unknown> = {};
  for (const [name, field] of Object.entries(fields)) {
    if (name in values) decoded[name] = field.codec.decode(values[name]);
  }
  return decoded;
}

function sameValue(left: unknown, right: unknown): boolean {
  return (
    JSON.stringify(left, (_key, value) =>
      typeof value === "bigint" ? `${value}n` : value,
    ) ===
    JSON.stringify(right, (_key, value) =>
      typeof value === "bigint" ? `${value}n` : value,
    )
  );
}
