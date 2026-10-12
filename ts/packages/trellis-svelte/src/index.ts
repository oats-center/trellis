export { default as TrellisProvider } from "./components/TrellisProvider.svelte";
export type { TrellisProviderProps } from "./components/TrellisProvider.types.ts";
export type {
  CreateTrellisAppOptions,
  SvelteTrellisConnection,
  TrellisApp,
  TrellisAppOwner,
  TrellisAppUrl,
  TrellisAppUrlResolver,
  TrellisClientFor,
  TrellisContextClient,
  TrellisAppRequest,
} from "./context.svelte.ts";
export { createTrellisApp, resolveTrellisAppUrl } from "./context.svelte.ts";
export { OwnAuthorityController } from "./own_authority.svelte.ts";
