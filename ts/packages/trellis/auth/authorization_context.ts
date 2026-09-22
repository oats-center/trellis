/**
 * Authorization context facade.
 *
 * The implementation is split by responsibility under `auth/authorization/`;
 * this module preserves the existing import surface for Trellis clients.
 */

export {
  AuthorizationContextCache,
  authorizationContextVerificationPolicy,
  verifyAuthorizationContext,
} from "./authorization/client_context.ts";
export {
  AuthorizationProviderCache,
  type AuthorizationProviderCacheHealth,
  type AuthorizationProviderCacheOptions,
  type AuthorizationProviderIoCounters,
} from "./authorization/provider_cache.ts";
export {
  AuthorizationContextRefreshError,
  refreshAuthorizationContext,
  refreshAuthorizationContextWithMetadata,
  startAuthorizationContextRefresh,
} from "./authorization/refresh.ts";
export type {
  AuthorizationContextBundle,
  AuthorizationContextRefreshResponse,
  AuthorizationContextRefreshResult,
  AuthorizationContextVerificationMaterial,
  AuthorizationProviderEvent,
  AuthorizationProviderRequest,
  AuthorizationRoutingMaterial,
  AuthorizationRuntimeBinding,
  AuthorizationRuntimeTransports,
  VerifiedAuthorizationContext,
} from "./authorization/types.ts";
export {
  AuthorizationContextBundleSchema,
  AuthorizationContextRefreshResponseSchema,
} from "./authorization/types.ts";
