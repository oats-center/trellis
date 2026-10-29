import {
  AsyncResult,
  Result,
  UnexpectedError,
  isErr,
} from "@oatscenter/result";
import {
  BrowserOAuthClient,
  type BrowserOAuthAdmission,
} from "./auth/browser/oauth.ts";
import { connectAdmitted } from "./auth/connection.ts";
import {
  initializeProtocolWasm,
  verifyOAuthAdmissionWasm,
} from "./auth/protocol_wasm.ts";
import { utf8 } from "./auth/utils.ts";
import type { RuntimeTransport } from "./runtime_transport.ts";
import {
  observeTrellisConnection,
  transitionConnectionAvailability,
} from "./connection.ts";
import { getCallerRuntime } from "./participant_runtime/participant.ts";
import type { AppRequestDescriptor } from "./generated_support.ts";
import type { OwnAuthorityView } from "./own_authority.ts";
import type { RuntimeApi } from "./participant_runtime/api.ts";
import { createCallerRuntime, type CallerRuntime } from "./caller.ts";
import type { ClientOpts } from "./client.ts";
import {
  Trellis,
  type RuntimeStateStores,
  type TrellisOpts,
} from "./session.ts";
import type { ResourceMigrations } from "./kv.ts";
import { AuthError } from "./errors/AuthError.ts";
import { TransportError } from "./errors/TransportError.ts";

/** Ordinary app OAuth identity and callback; no caller-authored participant wire claims. */
export type ClientAuthOptions = {
  clientId: string;
  /** Local multi-account slot; defaults to the registered client ID. */
  credentialHandle?: string;
  redirectTo: string;
  currentUrl?: string;
};
/** Required authorization redirect, returned to the application's own navigation owner. */
export type ClientAuthRequiredContext = { loginUrl: string; mode: "browser" };
/** A caller may consume navigation without the SDK changing the page. */
export type ClientAuthContinuation = { status: "handled" } | void;
/** Resource migrations are local application configuration, never authorization claims. */
export type ClientResourceMigrations<
  T extends AppRequestDescriptor = AppRequestDescriptor,
> = {
  state?: Partial<Record<keyof T["states"], ResourceMigrations<unknown>>>;
};
/** Browser app connection: HTTPS-selected OAuth issuer plus generated local descriptors. */
export type TrellisClientConnectArgs<
  T extends AppRequestDescriptor = AppRequestDescriptor,
> = ClientOpts & {
  trellisUrl: string;
  request: T;
  auth: ClientAuthOptions;
  resourceMigrations?: ClientResourceMigrations<T>;
  onAuthRequired?(
    context: ClientAuthRequiredContext,
  ): Promise<ClientAuthContinuation> | ClientAuthContinuation;
};
/** Options for ending a browser app's session. */
export type ClientLogoutOptions = {
  /**
   * Also end the Portal login on this browser, then navigate back to
   * `returnTo`. The next sign-in asks for credentials, so a person can switch
   * accounts, and nobody stays signed in to Trellis on a shared computer.
   */
  endPortalLogin?: boolean;
  /**
   * Where the Portal returns after ending its login; defaults to the current
   * page. Its origin must be one the app registered a redirect URI on.
   */
  returnTo?: string;
};
/** Connected ordinary OAuth app. Durable credentials remain in the app's IndexedDB realm. */
export type ConnectedTrellisClient<T extends AppRequestDescriptor> =
  CallerRuntime<T> & {
    readonly ownAuthority: OwnAuthorityView;
    /**
     * Revoke this application's OAuth family and close its connection. The
     * Portal login stays unless `endPortalLogin` is set, in which case the
     * browser navigates through the Portal's sign-out and back.
     */
    logout(options?: ClientLogoutOptions): Promise<void>;
  };

/** Connect through PKCE/DPoP OAuth followed by signed ephemeral admission. */
export function connectClient<T extends AppRequestDescriptor>(
  args: TrellisClientConnectArgs<T>,
) {
  return connectClientWithDeps(args, {});
}

/** Internal transport injection; uses the unchanged production OAuth and admission path. */
export function connectClientWithDeps<T extends AppRequestDescriptor>(
  args: TrellisClientConnectArgs<T>,
  deps: { transport?: RuntimeTransport },
) {
  return AsyncResult.from(
    (async (): Promise<
      Result<
        ConnectedTrellisClient<T>,
        AuthError | TransportError | UnexpectedError
      >
    > => {
      try {
        if (args.request.kind !== "browser")
          throw new AuthError({ reason: "InvalidCredential" });
        await initializeProtocolWasm();
        const opened = (
          await BrowserOAuthClient.open({
            issuer: args.trellisUrl,
            clientId: args.auth.clientId,
            credentialHandle: args.auth.credentialHandle ?? args.auth.clientId,
          })
        ).take();
        if (isErr(opened)) return Result.err(opened.error);
        const oauth = opened;
        let connected: Awaited<ReturnType<typeof connectAdmitted>> | undefined;
        const connectOAuth = async (admission: BrowserOAuthAdmission) => {
          let credential = admission.admission;
          const servers = admission.metadata.trellis.natsEndpoints.filter(
            (endpoint) =>
              endpoint.startsWith("ws://") || endpoint.startsWith("wss://"),
          );
          if (servers.length === 0)
            throw new AuthError({ reason: "InvalidIssuerMetadata" });
          const value = await connectAdmitted({
            servers,
            trellisInstanceId: admission.trust.instanceId,
            audienceNatsAccount: admission.trust.account,
            issuers: [admission.trust.issuer],
            rotationSequence: admission.trust.rotationSequence,
            auth: admission.auth,
            runtimeId: admission.runtimeId,
            kind: "user",
            timeout: args.timeout,
            signal: args.signal,
            transport: deps.transport,
            admission: async () => {
              const verified = await verifyOAuthAdmissionWasm(
                utf8(credential),
                {
                  issuer: (connected?.owner.trustSnapshot ?? admission.trust)
                    .issuer,
                  trellisInstanceId: admission.trust.instanceId,
                  audienceNatsAccount: admission.trust.account,
                  ephemeralNkey: admission.auth.sessionNkey,
                  nowUnixSeconds: admission.auth.currentIat(),
                  maximumLifetimeSeconds: 300,
                },
              );
              if (verified.ok) return credential;
              const next = (await oauth.resume()).take();
              if (isErr(next)) throw next.error;
              credential = next.admission;
              return credential;
            },
          }).catch((cause: unknown) => {
            // Name the endpoints so a failed dial can be traced to its address.
            throw cause instanceof AuthError || cause instanceof TransportError
              ? cause
              : new UnexpectedError({ cause }).withContext({ servers });
          });
          try {
            // The shared path matches signed OAuth identity before promotion.
            // Persist only that owner's authenticated successor chain.
            await oauth.bindOwner(value.owner);
            connected = value;
          } catch (error) {
            await value.manager.close();
            throw error;
          }
        };
        let refreshToken: string | undefined;
        const currentUrl = new URL(args.auth.currentUrl ?? location.href);
        const callback =
          currentUrl.searchParams.has("state") &&
          (currentUrl.searchParams.has("code") ||
            currentUrl.searchParams.has("error"));
        const resumed = callback
          ? (await oauth.complete(currentUrl.href)).take()
          : (await oauth.resume(connectOAuth)).take();
        if (isErr(resumed)) {
          await connected?.manager.close();
          if (resumed.error.reason !== "OAuthRecoveryRequired")
            return Result.err(resumed.error);
          const authorization = (
            await oauth.authorize(args.auth.redirectTo, {
              required: args.request.requiredCapabilities,
              optional: args.request.optionalCapabilities,
            })
          ).take();
          if (isErr(authorization)) return Result.err(authorization.error);
          if (args.onAuthRequired)
            await args.onAuthRequired({
              loginUrl: authorization,
              mode: "browser",
            });
          else location.assign(authorization);
          return Result.err(
            new AuthError({ reason: "OAuthAuthorizationRequired" }),
          );
        }
        const admission = "value" in resumed ? resumed.value : resumed;
        if ("refreshToken" in resumed) refreshToken = resumed.refreshToken;
        if (!connected) await connectOAuth(admission);
        if (!connected) throw new AuthError({ reason: "InvalidCredential" });
        const activeConnection = connected;
        try {
          const lease = activeConnection.owner.own();
          try {
            if (
              lease.authority.binding.kind !== "browser" ||
              lease.authority.binding.clientId !== args.auth.clientId ||
              lease.authority.binding.origin !== location.origin ||
              lease.authority.principalId !==
                admission.verifiedAdmission.credential.principalId ||
              lease.authority.oauthGrantId !==
                admission.verifiedAdmission.credential.oauthGrantId ||
              lease.authority.loginSessionId !==
                admission.verifiedAdmission.credential.loginSessionId
            )
              throw new AuthError({ reason: "InvalidCredential" });
            if (refreshToken) {
              if (
                !lease.authority.oauthGrantId ||
                !lease.authority.loginSessionId
              )
                throw new AuthError({ reason: "InvalidCredential" });
              const remembered = (
                await oauth.store.rememberGrant({
                  principalId: lease.authority.principalId,
                  oauthGrantId: lease.authority.oauthGrantId,
                  loginSessionId: lease.authority.loginSessionId,
                  refreshToken,
                })
              ).take();
              if (isErr(remembered)) throw remembered.error;
            }
          } finally {
            lease.release();
          }
          const runtime = getCallerRuntime(args.request);
          const connection = observeTrellisConnection({
            kind: "client",
            transport: {
              status: () => activeConnection.manager.status(),
              closed: () => activeConnection.manager.closed(),
              close: () => activeConnection.manager.close(),
              isClosed: () => activeConnection.manager.isClosed(),
              getServer: () =>
                activeConnection.manager.currentNats().getServer(),
            },
            log: false,
          });
          transitionConnectionAvailability(connection, true, "connected");
          const trellis = new Trellis<RuntimeApi, "client", RuntimeStateStores>(
            args.name ?? args.request.id,
            activeConnection.manager.currentNats(),
            {
              sessionKey: admission.auth.sessionKey,
              sign: admission.auth.sign,
              contextDigest: () => activeConnection.owner.ownDigest ?? "",
              authorityOwner: activeConnection.owner,
            },
            {
              api: runtime.usedApi as RuntimeApi,
              connection,
              transport: activeConnection.manager,
              timeout: args.timeout,
              log: args.log,
              stream: args.stream,
              state: runtime.state as TrellisOpts<RuntimeApi>["state"],
              stateMigrations: args.resourceMigrations?.state,
            },
            activeConnection.inboxPrefix,
          );
          const ownAuthority = trellis.ownAuthority;
          if (!ownAuthority) throw new AuthError({ reason: "Unavailable" });
          await ownAuthority.refresh();
          if (!ownAuthority.current)
            throw new AuthError({ reason: "Unavailable" });
          return Result.ok(
            Object.assign(createCallerRuntime(trellis, args.request), {
              ownAuthority,
              async logout(options?: ClientLogoutOptions) {
                const revoked = await oauth.revoke();
                if (revoked.isErr()) throw revoked.error;
                await activeConnection.manager.close();
                if (!options?.endPortalLogin) return;
                const signOut = new URL(oauth.metadata.end_session_endpoint);
                signOut.searchParams.set("client_id", args.auth.clientId);
                signOut.searchParams.set(
                  "post_logout_redirect_uri",
                  options.returnTo ?? location.href,
                );
                location.assign(signOut);
              },
            }),
          );
        } catch (error) {
          await activeConnection.manager.close();
          throw error;
        }
      } catch (cause) {
        return Result.err(
          cause instanceof AuthError ||
            cause instanceof TransportError ||
            cause instanceof UnexpectedError
            ? cause
            : new UnexpectedError({ cause }),
        );
      }
    })(),
  );
}

/** Public OAuth app connector. Requests choose local surfaces; signed admission owns authority. */
export class TrellisClient {
  /** Open a registered browser application using its generated request descriptor. */
  static connect<const T extends AppRequestDescriptor>(
    args: TrellisClientConnectArgs<T>,
  ) {
    return connectClient(args);
  }
}
