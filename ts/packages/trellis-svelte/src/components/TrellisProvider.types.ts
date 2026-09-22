import type {
  ClientAuthOptions,
  ClientAuthRequiredContext,
  ClientOpts,
} from "@qlever-llc/trellis";
import type { Snippet } from "svelte";
import type {
  TrellisAppOwner,
  TrellisParticipantLike,
} from "../context.svelte.ts";

/** Props accepted by the Svelte Trellis provider component. */
export type TrellisProviderProps<
  TContract extends TrellisParticipantLike = TrellisParticipantLike,
> = {
  trellisApp: TrellisAppOwner<TContract>;
  auth?: ClientAuthOptions;
  client?: ClientOpts;
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
