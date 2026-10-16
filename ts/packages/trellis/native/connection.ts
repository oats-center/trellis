import {
  AsyncResult,
  Result,
  UnexpectedError,
  isErr,
} from "@oatscenter/result";
import { connectAdmitted } from "../auth/connection.ts";
import { verifyOAuthAdmissionWasm } from "../auth/protocol_wasm.ts";
import { utf8 } from "../auth/utils.ts";
import { loadDefaultRuntimeTransport } from "../runtime_transport.ts";
import type { AppRequestDescriptor } from "../generated_support.ts";
import type { ClientOpts } from "../client.ts";
import type { ClientResourceMigrations } from "../client_connect.ts";
import { createCallerRuntime, type CallerRuntime } from "../caller.ts";
import { getCallerRuntime } from "../participant_runtime/participant.ts";
import type { RuntimeApi } from "../participant_runtime/api.ts";
import {
  Trellis,
  type RuntimeStateStores,
  type TrellisOpts,
} from "../session.ts";
import {
  observeTrellisConnection,
  transitionConnectionAvailability,
} from "../connection.ts";
import type { OwnAuthorityView } from "../own_authority.ts";
import { AuthError } from "../errors/AuthError.ts";
import { TransportError } from "../errors/TransportError.ts";
import type { NativeOAuthSession } from "./oauth.ts";

/** The shared admitted connection: one authority owner and ordinary generation manager. */
export type NativeSessionConnection = Awaited<
  ReturnType<typeof connectAdmitted>
>;

/** Connect a verified native OAuth session through the unchanged admission/authority/transport pipeline. Consumes session connection ownership. */
export function connectNativeSession(
  options: ClientOpts & {
    session: NativeOAuthSession;
    servers: readonly string[];
  },
): AsyncResult<
  NativeSessionConnection,
  AuthError | TransportError | UnexpectedError
> {
  return AsyncResult.from(
    (async () => {
      const session = options.session;
      let custody:
        ReturnType<NativeOAuthSession["claimConnection"]> | undefined;
      let connected: NativeSessionConnection | undefined;
      try {
        custody = session.claimConnection();
        let admission = session.admission;
        const trust = session.trust;
        connected = await connectAdmitted({
          servers: [...options.servers],
          trellisInstanceId: trust.instanceId,
          audienceNatsAccount: trust.account,
          issuers: [trust.issuer],
          rotationSequence: trust.rotationSequence,
          auth: session.auth,
          runtimeId: session.runtimeId,
          kind: "user",
          timeout: options.timeout,
          signal: options.signal,
          transport: await loadDefaultRuntimeTransport(),
          admission: async () => {
            // A transport attempt does not spend OAuth custody. Reuse only
            // admission the shared verifier still accepts under current pins.
            const verified = await verifyOAuthAdmissionWasm(utf8(admission), {
              issuer: (connected?.owner.trustSnapshot ?? trust).issuer,
              trellisInstanceId: trust.instanceId,
              audienceNatsAccount: trust.account,
              ephemeralNkey: session.auth.sessionNkey,
              nowUnixSeconds: session.auth.currentIat(),
              maximumLifetimeSeconds: 300,
            });
            if (verified.ok) return admission;
            if (!connected)
              throw new AuthError({ reason: "OAuthRecoveryRequired" });
            const refreshed = (
              await session.refresh({
                signal: options.signal,
              })
            ).take();
            if (isErr(refreshed)) throw refreshed.error;
            admission = refreshed.admission;
            return admission;
          },
        });
        custody.bindOwner(connected.owner);
        const own = connected.owner.own();
        try {
          if (
            own.authority.binding.kind !== "native" ||
            own.authority.binding.clientId !== session.scope.clientId ||
            own.authority.binding.durableDpopJkt !== session.durableDpopJkt ||
            own.authority.loginSessionId !== session.loginSessionId ||
            own.authority.oauthGrantId !== session.oauthGrantId ||
            own.authority.principalId !== session.principalId
          )
            throw new AuthError({ reason: "InvalidCredential" });
        } catch (error) {
          await connected.manager.close();
          throw error;
        } finally {
          own.release();
        }
        void connected.manager
          .closed()
          .then(() => custody?.close())
          .catch(() => undefined);
        return Result.ok(connected);
      } catch (cause) {
        await connected?.manager.close();
        await custody?.close();
        return Result.err(
          cause instanceof AuthError || cause instanceof TransportError
            ? cause
            : new UnexpectedError({ cause }),
        );
      }
    })(),
  );
}

/** Native generated facade options. Application requests select local surfaces, never wire authorization. */
export type NativeClientConnectOptions<T extends AppRequestDescriptor> =
  ClientOpts & {
    request: T;
    session: NativeOAuthSession;
    servers: readonly string[];
    resourceMigrations?: ClientResourceMigrations<T>;
  };

/** Ordinary generated caller with the shared promoted own-authority view. */
export type ConnectedNativeClient<T extends AppRequestDescriptor> =
  CallerRuntime<T> & {
    readonly ownAuthority: OwnAuthorityView;
    /** Refresh authenticated authority through the shared connection owner. */
    refreshAuthority(): Promise<void>;
  };

/** Open a generated native application using exclusively owned, issuer-verified OAuth custody. */
export function connectNativeClient<const T extends AppRequestDescriptor>(
  options: NativeClientConnectOptions<T>,
): AsyncResult<
  ConnectedNativeClient<T>,
  AuthError | TransportError | UnexpectedError
> {
  return AsyncResult.from(
    (async () => {
      if (options.request.kind !== "native")
        return Result.err(new AuthError({ reason: "InvalidCredential" }));
      const result = (await connectNativeSession(options)).take();
      if (isErr(result)) return Result.err(result.error);
      const connected = result;
      try {
        const runtime = getCallerRuntime(options.request);
        let readyView: OwnAuthorityView | undefined;
        const connection = observeTrellisConnection({
          kind: "client",
          transport: {
            status: async function* () {
              for await (const status of connected.manager.status()) {
                const view = readyView;
                if (
                  status &&
                  typeof status === "object" &&
                  Reflect.get(status, "type") === "reconnect" &&
                  view &&
                  !view.current
                ) {
                  // The logical attachment is covered before reconnect, but
                  // its verified advisory projection completes asynchronously.
                  // Publish client readiness only after both are installed.
                  let unsubscribe = () => {};
                  try {
                    const projected = new Promise<boolean>((resolve) => {
                      unsubscribe = view.subscribe((snapshot) => {
                        if (snapshot) resolve(true);
                      });
                    });
                    if (
                      !(await Promise.race([
                        projected,
                        connected.manager.closed().then(() => false),
                      ]))
                    )
                      return;
                  } finally {
                    unsubscribe();
                  }
                }
                yield status;
              }
            },
            closed: () => connected.manager.closed(),
            close: () => connected.manager.close(),
            isClosed: () => connected.manager.isClosed(),
            getServer: () => connected.manager.currentNats().getServer(),
          },
          log: false,
        });
        transitionConnectionAvailability(connection, true, "connected");
        const trellis = new Trellis<RuntimeApi, "client", RuntimeStateStores>(
          options.name ?? options.request.id,
          connected.manager.currentNats(),
          {
            sessionKey: connected.auth.sessionKey,
            sign: connected.auth.sign,
            contextDigest: () => connected.owner.ownDigest ?? "",
            authorityOwner: connected.owner,
          },
          {
            api: runtime.usedApi as RuntimeApi,
            connection,
            transport: connected.manager,
            timeout: options.timeout,
            log: options.log,
            stream: options.stream,
            state: runtime.state as TrellisOpts<RuntimeApi>["state"],
            stateMigrations: options.resourceMigrations?.state,
          },
          connected.inboxPrefix,
        );
        const ownAuthority = trellis.ownAuthority;
        if (!ownAuthority) throw new AuthError({ reason: "Unavailable" });
        readyView = ownAuthority;
        await ownAuthority.refresh();
        if (!ownAuthority.current)
          throw new AuthError({ reason: "Unavailable" });
        return Result.ok(
          Object.assign(createCallerRuntime(trellis, options.request), {
            ownAuthority,
            refreshAuthority: async () => {
              await connected.refreshAuthority();
              await ownAuthority.refresh();
            },
          }),
        );
      } catch (cause) {
        await connected.manager.close();
        return Result.err(
          cause instanceof AuthError || cause instanceof TransportError
            ? cause
            : new UnexpectedError({ cause }),
        );
      }
    })(),
  );
}
