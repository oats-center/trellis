import type {
  ClientAuthOptions,
  ClientAuthRequiredContext,
  ClientOpts,
} from "@oatscenter/trellis";
import type { Snippet } from "svelte";
import type {
  TrellisAppOwner,
  TrellisAppRequest,
  TrellisClientFor,
} from "../context.svelte.ts";

/** Props accepted by the Svelte Trellis provider component. */
export type TrellisProviderProps<
  TContract extends TrellisAppRequest = TrellisAppRequest,
> = {
  trellisApp: TrellisAppOwner<TContract>;
  auth: ClientAuthOptions;
  client?: ClientOpts;
  /**
   * A client already connected for this app, such as the one a sign-in
   * callback page connected before navigating on. The provider uses it
   * instead of connecting again, and closes it when it unmounts.
   */
  connected?: TrellisClientFor<TContract>;
  children: Snippet;
  loading?: Snippet;
  recoveringAuth?: Snippet;
  error?: Snippet<[unknown]>;
  onAuthRequired?: (
    loginUrl: string,
    context: ClientAuthRequiredContext,
  ) => void | Promise<void>;
  onRecoverableAuthError?: (error: unknown) => void | Promise<void>;
};
