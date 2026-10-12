/** Node-only native Device Code OAuth, protected POSIX custody and ordinary generated callers. @module */
export {
  discoverNativeOAuth,
  NativeOAuthClient,
  NativeOAuthSession,
  NativeDeviceAuthorization,
  type NativeOAuthServer,
} from "./native/oauth.ts";
export type { NativeCredentialScope } from "./native/storage.ts";
export type { RuntimeAuthorizationTrust } from "./auth/authority.ts";
export type { PlatformPrivilege } from "./auth/protocol_wasm.ts";
export {
  connectNativeSession,
  connectNativeClient,
  type NativeSessionConnection,
  type NativeClientConnectOptions,
  type ConnectedNativeClient,
} from "./native/connection.ts";
