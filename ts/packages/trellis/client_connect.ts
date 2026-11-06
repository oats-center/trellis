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
   * How to ask when the app uses an optional capability the user hasn't
   * approved: leave for the approval page and come back (`"redirect"`, the
   * default), or open it in a popup and carry on with the call once approved
   * (`"popup"`). A blocked popup falls back to a redirect. Asks on page load
   * always redirect.
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
        // reports where it landed.
        const landed = new URL(args.auth.currentUrl ?? location.href);
        if (landed.searchParams.has("state") && sameOriginOpener()) {
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
        const install = async (admission: BrowserOAuthAdmission) => {
          const details = admission.authorizationDetails;
          const granted = details?.flatMap((detail) => detail.required);
          outdated =
            granted !== undefined &&
            args.request.requiredCapabilities.some(
              (capability) => !granted.includes(capability),
            );
          const requested = details?.every(
            (detail) => detail.requested_optional !== undefined,
          )
            ? details.flatMap((detail) => detail.requested_optional ?? [])
            : undefined;
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
              popup.close();
            }
          };
          let asking: Promise<boolean> | undefined;
          const requestApproval: CallerApproval = () => {
            if (asking) return asking;
            // A popup only opens in the same turn as the user's action.
            const popup =
              args.auth.approval === "popup"
                ? window.open("about:blank", APPROVAL_MESSAGE, POPUP_FEATURES)
                : null;
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
              createCallerRuntime(trellis, args.request, {}, requestApproval),
              {
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

/** Public OAuth app connector. Requests choose local surfaces; signed admission owns authority. */
export class TrellisClient {
  /** Open a registered browser application using its generated request descriptor. */
  static connect<const T extends AppRequestDescriptor>(
    args: TrellisClientConnectArgs<T>,
  ) {
    return connectClient(args);
  }
}

/** Names the approval popup and the message it sends its opener. */
const APPROVAL_MESSAGE = "trellis.approval";
const POPUP_FEATURES = "popup,width=520,height=720";

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
