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
import {
  createCallerRuntime,
  type CallerApproval,
  type CallerRuntime,
} from "./caller.ts";
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
  /**
   * Ask the user to approve an optional capability when they use it: leave
   * for the approval page and come back (`"redirect"`), or open it in a popup
   * and carry on with the call once approved (`"popup"`). Only calls made in
   * response to the user's click or key press ask; calls the app makes on its
   * own, such as loading a page, fail with `ActionUnavailable`. Unset, no call
   * asks. A blocked popup falls back to a redirect.
   */
  approval?: "redirect" | "popup";
  /**
   * On page load, ask about optional capabilities the app requests that the
   * user has never been asked about, such as ones a new version added. Off,
   * they are asked for when the app first uses them.
   */
  askForNewOptional?: boolean;
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
        // An approval popup ends on the app's callback. The window that opened
        // it holds the continuation and reads the outcome; this one only
        // reports where it landed. Any other window, opened by the app or
        // not, completes its own sign-in.
        const landed = new URL(args.auth.currentUrl ?? location.href);
        const state = landed.searchParams.get("state");
        if (state && sameOriginOpener() && approvalPending(state)) {
          window.opener.postMessage(
            { type: APPROVAL_MESSAGE, href: landed.href },
            location.origin,
          );
          return Result.err(new AuthError({ reason: "OAuthApprovalWindow" }));
        }
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
            natsSentinel: admission.metadata.trellis.natsSentinel,
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
        // A grant keeps the capabilities approved for it, so refreshing one
        // that lacks a capability the app now requires never adds it. Such a
        // grant is revoked and the user approves again; the approval screen
        // keeps their earlier choices and asks only about what is new.
        let outdated = false;
        let unseen: string[] = [];
        // The optional capabilities the user was shown when approving this grant.
        let grantAsked: readonly string[] = [];
        const requestedOptional = (admission: BrowserOAuthAdmission) => {
          const details = admission.authorizationDetails;
          return details?.every(
            (detail) => detail.requested_optional !== undefined,
          )
            ? details.flatMap((detail) => detail.requested_optional ?? [])
            : undefined;
        };
        const install = async (admission: BrowserOAuthAdmission) => {
          const details = admission.authorizationDetails;
          // An optional capability the user approved counts once a newer app
          // version requires it: the grant already holds it.
          const granted = details?.flatMap((detail) => [
            ...detail.required,
            ...detail.optional,
          ]);
          outdated =
            granted !== undefined &&
            args.request.requiredCapabilities.some(
              (capability) => !granted.includes(capability),
            );
          const requested = requestedOptional(admission);
          grantAsked = requested ?? [];
          if (!outdated && args.auth.askForNewOptional && requested) {
            const asked = new Set([...requested, ...askedOptional(oauth)]);
            unseen = args.request.optionalCapabilities.filter(
              (capability) => !asked.has(capability),
            );
          }
          if (!outdated && unseen.length === 0) await connectOAuth(admission);
        };
        let resumed = callback
          ? (await oauth.complete(currentUrl.href)).take()
          : (await oauth.resume(install)).take();
        // The consent just given is what the grant asked about.
        if (callback && !isErr(resumed) && "value" in resumed)
          grantAsked = requestedOptional(resumed.value) ?? [];
        // Declining more leaves the app as it was, on the grant already approved.
        if (
          callback &&
          isErr(resumed) &&
          resumed.error.reason === "OAuthAuthorizationDenied"
        ) {
          const kept = (await oauth.resume(install)).take();
          if (!isErr(kept)) resumed = kept;
        }
        if (!isErr(resumed) && outdated) {
          const revoked = await oauth.revoke();
          if (revoked.isErr()) return Result.err(revoked.error);
        }
        if (isErr(resumed) || outdated || unseen.length > 0) {
          await connected?.manager.close();
          if (
            isErr(resumed) &&
            resumed.error.reason !== "OAuthRecoveryRequired"
          )
            return Result.err(resumed.error);
          // Asked once: declining leaves them unapproved without asking again.
          if (unseen.length > 0) rememberAsked(oauth, unseen);
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
          const request = {
            required: args.request.requiredCapabilities,
            optional: args.request.optionalCapabilities,
          };
          const redirectForApproval = async () => {
            const url = (
              await oauth.authorize(args.auth.redirectTo, request)
            ).take();
            if (isErr(url)) return;
            if (args.onAuthRequired)
              await args.onAuthRequired({ loginUrl: url, mode: "browser" });
            else location.assign(url);
          };
          // The popup widens this session's own grant, so the live
          // connection renews into the approved capabilities in place.
          const approveInPopup = async (popup: Window): Promise<boolean> => {
            let state: string | undefined;
            try {
              const lease = activeConnection.owner.own();
              const grant = lease.authority.oauthGrantId;
              lease.release();
              if (!grant) return false;
              const url = (
                await oauth.authorize(args.auth.redirectTo, request, {
                  extendGrant: grant,
                })
              ).take();
              if (isErr(url)) return false;
              state = new URL(url).searchParams.get("state") ?? undefined;
              if (state) markApproval(state, true);
              popup.location.href = url;
              const landed = await approvalCallback(
                popup,
                new URL(args.auth.redirectTo).pathname,
              );
              if (!landed) return false;
              const approved = oauth.approvalOutcome(landed).take();
              if (isErr(approved) || !approved) return false;
              await activeConnection.refreshAuthority();
              await ownAuthority.refresh();
              return true;
            } catch {
              return false;
            } finally {
              if (state) markApproval(state, false);
              popup.close();
            }
          };
          let asking: Promise<boolean> | undefined;
          const requestApproval: CallerApproval = (capabilities) => {
            // Only a person's action asks. A call the app makes by itself
            // would ask again on every load, even for a capability the user
            // can never hold.
            if (!args.auth.approval || !navigator.userActivation?.isActive)
              return undefined;
            if (asking) return asking;
            // A popup only opens in the same turn as the user's action.
            const popup =
              args.auth.approval === "popup"
                ? window.open("about:blank", APPROVAL_MESSAGE, POPUP_FEATURES)
                : null;
            rememberAsked(oauth, [...capabilities]);
            if (!popup) {
              void redirectForApproval();
              return Promise.resolve(false);
            }
            asking = approveInPopup(popup).finally(() => {
              asking = undefined;
            });
            return asking;
          };
          return Result.ok(
            Object.assign(
              createCallerRuntime(
                trellis,
                args.request,
                {},
                requestApproval,
                () => [...grantAsked, ...askedOptional(oauth)],
              ),
              {
                ownAuthority,
                async logout(options?: ClientLogoutOptions) {
                  const revoked = await oauth.revoke();
                  if (revoked.isErr()) throw revoked.error;
                  await activeConnection.manager.close();
                  if (!options?.endPortalLogin) return;
                  endPortalLogin(oauth, args.auth.clientId, options.returnTo);
                },
              },
            ),
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

/** Navigate through the Portal's sign-out, which returns to `returnTo`. */
function endPortalLogin(
  oauth: BrowserOAuthClient,
  clientId: string,
  returnTo = location.href,
): void {
  const signOut = new URL(oauth.metadata.end_session_endpoint);
  signOut.searchParams.set("client_id", clientId);
  signOut.searchParams.set("post_logout_redirect_uri", returnTo);
  location.assign(signOut);
}

/** Options for signing a browser app out without connecting it first. */
export type ClientSignOutArgs = {
  trellisUrl: string;
  auth: Pick<ClientAuthOptions, "clientId" | "credentialHandle">;
  /**
   * Where the Portal returns after ending its login; defaults to the current
   * page. Its origin must be one the app registered a redirect URI on.
   */
  returnTo?: string;
};

/** Public OAuth app connector. Requests choose local surfaces; signed admission owns authority. */
export class TrellisClient {
  /** Open a registered browser application using its generated request descriptor. */
  static connect<const T extends AppRequestDescriptor>(
    args: TrellisClientConnectArgs<T>,
  ) {
    return connectClient(args);
  }

  /**
   * Revoke the app's stored OAuth session and end the Portal login, then
   * navigate back to `returnTo`. Unlike `logout` on a connected client, this
   * works when connecting failed, so a person can start over and sign in
   * again, possibly as someone else.
   */
  static signOut(args: ClientSignOutArgs): AsyncResult<void, AuthError> {
    return AsyncResult.from(
      (async (): Promise<Result<void, AuthError>> => {
        const opened = (
          await BrowserOAuthClient.open({
            issuer: args.trellisUrl,
            clientId: args.auth.clientId,
            credentialHandle: args.auth.credentialHandle ?? args.auth.clientId,
          })
        ).take();
        if (isErr(opened)) return Result.err(opened.error);
        const revoked = await opened.revoke();
        if (revoked.isErr()) {
          // A missing or unusable credential has no session left to revoke;
          // dropping it locally is enough.
          if (
            !["InvalidCredential", "OAuthRecoveryRequired"].includes(
              revoked.error.reason,
            )
          )
            return Result.err(revoked.error);
          await opened.store.clear();
        }
        endPortalLogin(opened, args.auth.clientId, args.returnTo);
        return Result.ok(undefined);
      })(),
    );
  }
}

/** Names the approval popup and the message it sends its opener. */
const APPROVAL_MESSAGE = "trellis.approval";
const POPUP_FEATURES = "popup,width=520,height=720";

/** Where an opener records the approval it sent a popup to, by OAuth state. */
const approvalKey = (state: string) => `trellis.approval:${state}`;

function markApproval(state: string, pending: boolean): void {
  try {
    if (pending) localStorage.setItem(approvalKey(state), "1");
    else localStorage.removeItem(approvalKey(state));
  } catch {
    // Without storage the opener still sees the popup land by watching it.
  }
}

function approvalPending(state: string): boolean {
  try {
    return localStorage.getItem(approvalKey(state)) !== null;
  } catch {
    return false;
  }
}

/** Whether this window was opened by another window of this app's origin. */
function sameOriginOpener(): boolean {
  try {
    return (
      window.opener !== null &&
      window.opener !== window &&
      window.opener.location.origin === location.origin
    );
  } catch {
    return false;
  }
}

/**
 * Resolves with the app callback URL `popup` reaches, from its message or by
 * watching it arrive, or undefined once the popup closes or the approval's
 * ten minutes run out.
 */
function approvalCallback(
  popup: Window,
  path: string,
): Promise<string | undefined> {
  return new Promise((resolve) => {
    const deadline = Date.now() + 600_000;
    const finish = (href: string | undefined) => {
      clearInterval(timer);
      removeEventListener("message", onMessage);
      resolve(href);
    };
    const onMessage = (event: MessageEvent<unknown>) => {
      const data = event.data;
      if (
        event.source === popup &&
        event.origin === location.origin &&
        typeof data === "object" &&
        data !== null &&
        "type" in data &&
        data.type === APPROVAL_MESSAGE &&
        "href" in data &&
        typeof data.href === "string"
      )
        finish(data.href);
    };
    addEventListener("message", onMessage);
    const timer = setInterval(() => {
      if (popup.closed || Date.now() > deadline) return finish(undefined);
      try {
        const at = new URL(popup.location.href);
        if (
          at.origin === location.origin &&
          at.pathname === path &&
          at.searchParams.has("state")
        )
          finish(at.href);
      } catch {
        // Still on the approval page, another origin.
      }
    }, 200);
  });
}

/** Where this app remembers the optional capabilities it has asked about. */
function askedKey(oauth: BrowserOAuthClient): string {
  const { issuer, clientId, credentialHandle } = oauth.scope;
  return `trellis.askedOptional:${JSON.stringify([issuer, clientId, credentialHandle])}`;
}

function askedOptional(oauth: BrowserOAuthClient): string[] {
  try {
    const value: unknown = JSON.parse(
      localStorage.getItem(askedKey(oauth)) ?? "[]",
    );
    return Array.isArray(value)
      ? value.filter((item) => typeof item === "string")
      : [];
  } catch {
    return [];
  }
}

function rememberAsked(oauth: BrowserOAuthClient, asked: string[]): void {
  try {
    localStorage.setItem(
      askedKey(oauth),
      JSON.stringify([...new Set([...askedOptional(oauth), ...asked])]),
    );
  } catch {
    // Without storage the app asks again on its next load.
  }
}
