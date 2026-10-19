/** Native Device Code OAuth with process-local or explicit application-owned custody. @module */
export {
  NativeOAuthClient,
  NativeOAuthSession,
  NativeDeviceAuthorization,
} from "./native/oauth.ts";
export {
  discoverNativeOAuth,
  type NativeOAuthServer,
} from "./native/discovery.ts";
export {
  MemoryNativeOAuthCustody,
  type NativeCredentialScope,
  type NativeOAuthCustody,
  type NativeOAuthCustodyLease,
  type NativeOAuthGrant,
} from "./native/custody.ts";
export type { RuntimeAuthorizationTrust } from "./auth/authority.ts";
export type { PlatformPrivilege } from "./auth/protocol_wasm.ts";
export {
  connectNativeSession,
  connectNativeClient,
  type NativeSessionConnection,
  type NativeClientConnectOptions,
  type ConnectedNativeClient,
} from "./native/connection.ts";
