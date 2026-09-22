import type {
  CallerParticipant,
  ConnectedTrellisClient,
  TrellisConnection,
  TrellisConnectionStatus,
} from "@qlever-llc/trellis";
import { createContext } from "svelte";
import { createSubscriber } from "svelte/reactivity";

/** Minimal contract shape required to create a typed Trellis Svelte app context. */
export type TrellisParticipantLike = CallerParticipant;

/** Real connected Trellis client type exposed by a Svelte app context. */
export type TrellisClientFor<TContract extends TrellisParticipantLike> =
  ConnectedTrellisClient<TContract>;

/** Minimal client surface required for Trellis Svelte context clients. */
export type TrellisContextClient = {
  readonly connection: TrellisConnection;
};

/** URL value accepted by a Trellis Svelte app owner. */
export type TrellisAppUrl = string | URL;

/**
 * Trellis URL configuration for a Svelte app owner.
 *
 * A function resolver supports apps whose Trellis instance is selected at
 * runtime. Returning `undefined` means no instance is currently available.
 */
export type TrellisAppUrlResolver =
  | TrellisAppUrl
  | (() => TrellisAppUrl | undefined);

/** Options used to create a Trellis Svelte app owner. */
export type CreateTrellisAppOptions<
  TContract extends TrellisParticipantLike = TrellisParticipantLike,
> = {
  /** Generated participant used by this app context and its connections. */
  participant: TContract;

  /** Trellis URL, or a resolver for runtime-selected Trellis URLs. */
  trellisUrl: TrellisAppUrlResolver;
};

/** Resolves a Trellis app URL config to the string shape required by the client. */
export function resolveTrellisAppUrl(
  trellisUrl: TrellisAppUrlResolver,
): string | undefined {
  const resolved = typeof trellisUrl === "function" ? trellisUrl() : trellisUrl;
  return resolved?.toString();
}

/** Svelte-reactive adapter around a framework-neutral Trellis connection. */
export class SvelteTrellisConnection {
  #connection: TrellisConnection;
  #subscribe: () => void;

  /** Creates a reactive connection adapter for a connected Trellis runtime. */
  constructor(connection: TrellisConnection) {
    this.#connection = connection;
    this.#subscribe = createSubscriber((update) => {
      return this.#connection.subscribe(() => update());
    });
  }

  /** Latest connection status, reactive when read by Svelte effects or markup. */
  get status(): TrellisConnectionStatus {
    this.#subscribe();
    return this.#connection.status;
  }

  /** Closes the underlying Trellis runtime connection. */
  close(): Promise<void> {
    return this.#connection.close();
  }
}

type TrellisAppContext<TClient extends TrellisContextClient> = {
  trellis: TClient;
  connection: SvelteTrellisConnection;
};

const provideTrellisContext = Symbol("provideTrellisContext");
const trellisAppOwnerBrand: unique symbol = Symbol("trellisAppOwner");

/** Minimal branded app owner surface accepted by the Trellis Svelte provider. */
export type TrellisAppOwner<
  TContract extends TrellisParticipantLike = TrellisParticipantLike,
> = {
  readonly participant: TContract;
  readonly trellisUrl: TrellisAppUrlResolver;
  readonly [trellisAppOwnerBrand]: true;
};

/**
 * Public app-scoped typed Svelte context owner for a Trellis browser app.
 *
 * `TClient` is a type-only refinement over the descriptor-inferred caller runtime.
 */
export interface TrellisApp<
  TContract extends TrellisParticipantLike = TrellisParticipantLike,
  TClient extends TrellisContextClient = TrellisClientFor<TContract>,
> extends TrellisAppOwner<TContract> {
  /** Generated participant used by this app context and its connections. */
  readonly participant: TContract;

  /** Trellis URL configuration used by `TrellisProvider` connections. */
  readonly trellisUrl: TrellisAppUrlResolver;

  /** Returns the contract-typed connected Trellis client from Svelte context synchronously. */
  getTrellis(): TClient;

  /** Returns a Svelte-reactive adapter for the real Trellis connection. */
  getConnection(): SvelteTrellisConnection;
}

/** Internal app-scoped typed Svelte context implementation. */
class TrellisAppImpl<
  TContract extends TrellisParticipantLike = TrellisParticipantLike,
  TClient extends TrellisContextClient = TrellisClientFor<TContract>,
> {
  readonly [trellisAppOwnerBrand] = true as const;
  readonly #participant: TContract;
  readonly #trellisUrl: TrellisAppUrlResolver;
  readonly #getContext: () => TrellisAppContext<TClient>;
  readonly #setContext: (
    context: TrellisAppContext<TClient>,
  ) => TrellisAppContext<TClient>;

  /** Creates an app-scoped context owner for a specific Trellis contract. */
  constructor(options: CreateTrellisAppOptions<TContract>) {
    const { participant, trellisUrl } = options;
    this.#participant = participant;
    this.#trellisUrl = trellisUrl;
    const [getContext, setContext] = createContext<
      TrellisAppContext<TClient>
    >();
    this.#getContext = getContext;
    this.#setContext = setContext;
  }

  /** Generated participant used by `TrellisProvider` connections. */
  get participant(): TContract {
    return this.#participant;
  }

  /** Trellis URL configuration used by `TrellisProvider` connections. */
  get trellisUrl(): TrellisAppUrlResolver {
    return this.#trellisUrl;
  }

  /** Returns the contract-typed connected Trellis client from Svelte context synchronously. */
  getTrellis(): TClient {
    return this.#getContext().trellis;
  }

  /** Returns a Svelte-reactive adapter for the real Trellis connection. */
  getConnection(): SvelteTrellisConnection {
    return this.#getContext().connection;
  }

  /** Installs the connected Trellis runtime into Svelte context synchronously. */
  [provideTrellisContext](trellis: TrellisContextClient): void {
    this.#setContext({
      trellis: trellis as TClient,
      connection: new SvelteTrellisConnection(trellis.connection),
    });
  }
}

/**
 * Creates an app-scoped typed Svelte context owner for a Trellis contract and URL.
 *
 * The optional `TClient` type parameter refines the connected caller runtime.
 */
export function createTrellisApp<
  TContract extends TrellisParticipantLike,
  TClient extends TrellisContextClient = TrellisClientFor<TContract>,
>(
  options: CreateTrellisAppOptions<TContract>,
): TrellisApp<TContract, TClient> {
  return new TrellisAppImpl<TContract, TClient>(options);
}

function isTrellisAppImpl(
  app: TrellisAppOwner,
): app is TrellisAppImpl<TrellisParticipantLike> {
  return app instanceof TrellisAppImpl;
}

/**
 * Internal provider helper that synchronously installs connected Trellis context.
 *
 * This is intentionally not exported from `src/index.ts`.
 */
export function provideConnectedTrellisContext(
  app: TrellisAppOwner,
  trellis: TrellisContextClient,
): void {
  if (!isTrellisAppImpl(app)) {
    throw new TypeError("Expected an app created by createTrellisApp");
  }
  app[provideTrellisContext](trellis);
}
