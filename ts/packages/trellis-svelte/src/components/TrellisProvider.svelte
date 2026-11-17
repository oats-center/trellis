<script lang="ts" generics="TContract extends TrellisAppRequest">
  import { classifyBrowserAuthError, TrellisClient } from "@oatscenter/trellis";
  import { onMount, untrack } from "svelte";
  import type {
    TrellisClientFor,
    TrellisAppRequest,
  } from "../context.svelte.ts";
  import { resolveTrellisAppUrl } from "../context.svelte.ts";
  import TrellisContextProvider from "./TrellisContextProvider.svelte";
  import type { TrellisProviderProps } from "./TrellisProvider.types.ts";

  const {
    trellisApp,
    auth,
    client,
    connected,
    children,
    loading,
    recoveringAuth,
    error: errorSnippet,
    onAuthRequired,
    onRecoverableAuthError,
  }: TrellisProviderProps<TContract> = $props();

  type ProviderState = "connecting" | "connected" | "auth_handled" | "failed";

  // A client connected before this provider mounted renders from the first
  // frame; a later change to the prop does not replace it.
  const adopted = untrack(() => connected);
  let trellis = $state<TrellisClientFor<TContract> | null>(adopted ?? null);
  let connectError = $state<unknown>(null);
  let providerState = $state<ProviderState>(
    adopted ? "connected" : "connecting",
  );

  type SerializableTrellisError = {
    message?: unknown;
    code?: unknown;
    hint?: unknown;
    context?: unknown;
  };

  function maybeSerializableError(
    value: unknown,
  ): SerializableTrellisError | undefined {
    if (!value || typeof value !== "object" || !("toSerializable" in value)) {
      return undefined;
    }
    const serialize = value.toSerializable;
    if (typeof serialize !== "function") return undefined;
    const serialized = serialize.call(value);
    return serialized && typeof serialized === "object"
      ? (serialized as SerializableTrellisError)
      : undefined;
  }

  function contextRecord(value: unknown): Record<string, unknown> | undefined {
    return value && typeof value === "object" && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : undefined;
  }

  function logConnectionError(error: unknown): void {
    const serialized = maybeSerializableError(error);
    const context = contextRecord(serialized?.context);
    const causeMessage =
      typeof context?.causeMessage === "string"
        ? context.causeMessage
        : undefined;
    const message =
      typeof serialized?.message === "string"
        ? serialized.message
        : error instanceof Error
          ? error.message
          : String(error);

    console.error("TrellisProvider failed to connect", {
      message,
      code: serialized?.code,
      hint: serialized?.hint,
      causeMessage,
      context: serialized?.context,
      error,
    });
  }

  onMount(() => {
    let active = true;
    const close = () => {
      active = false;
      const current = trellis;
      trellis = null;
      providerState = "connecting";
      if (current) {
        void current.connection.close();
      }
    };
    if (adopted) return close;

    const connectAuth = {
      ...auth,
      currentUrl: auth.currentUrl ?? window.location.href,
    };
    let authHandled = false;
    const trellisUrl = resolveTrellisAppUrl(trellisApp.trellisUrl);
    if (!trellisUrl) {
      connectError = new TypeError(
        "Expected trellisApp to resolve a Trellis URL",
      );
      providerState = "failed";
      return;
    }

    void (async () => {
      try {
        const connected = await TrellisClient.connect({
          ...client,
          trellisUrl,
          request: trellisApp.request,
          auth: connectAuth,
          onAuthRequired: onAuthRequired
            ? async (ctx) => {
                await onAuthRequired(ctx.loginUrl, ctx);
                authHandled = true;
                return { status: "handled" };
              }
            : undefined,
        }).orThrow();

        if (active) {
          trellis = connected;
          providerState = "connected";
        } else {
          await connected.connection.close();
        }
      } catch (error) {
        if (!active) return;
        if (authHandled) {
          providerState = "auth_handled";
          return;
        }
        const authRecovery = classifyBrowserAuthError(error);
        if (authRecovery.recoverable) {
          if (onRecoverableAuthError) {
            providerState = "auth_handled";
            try {
              await onRecoverableAuthError(error);
              return;
            } catch (recoveryError) {
              if (!active) return;
              console.error("TrellisProvider auth recovery callback failed", {
                recoveryError,
                error,
              });
              logConnectionError(recoveryError);
              connectError = recoveryError;
              providerState = "failed";
              return;
            }
          }
          if (recoveringAuth) {
            providerState = "auth_handled";
            return;
          }
        }
        logConnectionError(error);
        connectError = error;
        providerState = "failed";
      }
    })();

    return close;
  });
</script>

{#if trellis}
  <TrellisContextProvider {trellisApp} {trellis} {children} />
{:else if providerState === "failed" && connectError}
  {#if errorSnippet}
    {@render errorSnippet(connectError)}
  {/if}
{:else if providerState === "auth_handled" && recoveringAuth}
  {@render recoveringAuth()}
{:else if loading}
  {@render loading()}
{/if}
