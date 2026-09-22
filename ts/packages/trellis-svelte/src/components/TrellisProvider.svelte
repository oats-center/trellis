<script lang="ts" generics="TContract extends TrellisParticipantLike">
  import {
    classifyBrowserAuthError,
    ClientAuthHandledError,
    TrellisClient,
    type ClientAuthOptions,
  } from "@qlever-llc/trellis";
  import { onMount } from "svelte";
  import type {
    TrellisClientFor,
    TrellisParticipantLike,
  } from "../context.svelte.ts";
  import { resolveTrellisAppUrl } from "../context.svelte.ts";
  import TrellisContextProvider from "./TrellisContextProvider.svelte";
  import type { TrellisProviderProps } from "./TrellisProvider.types.ts";

  const {
    trellisApp,
    auth,
    client,
    children,
    loading,
    recoveringAuth,
    error: errorSnippet,
    onAuthRequired,
    onRecoverableAuthError,
  }: TrellisProviderProps<TContract> = $props();

  type ProviderState = "connecting" | "connected" | "auth_handled" | "failed";

  let trellis = $state<TrellisClientFor<TContract> | null>(null);
  let connectError = $state<unknown>(null);
  let providerState = $state<ProviderState>("connecting");

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

    function withBrowserAuthDefaults(
      authOptions: ClientAuthOptions | undefined,
    ): ClientAuthOptions {
      if (authOptions?.mode === "session_key") {
        return authOptions;
      }

      return {
        ...authOptions,
        currentUrl:
          authOptions?.currentUrl ?? (() => new URL(window.location.href)),
      };
    }

    const connectAuth = withBrowserAuthDefaults(auth);
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
          participant: trellisApp.participant,
          auth: connectAuth,
          onAuthRequired: onAuthRequired
            ? async (ctx) => {
                await onAuthRequired(ctx.loginUrl, ctx);
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
        if (error instanceof ClientAuthHandledError) {
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

    return () => {
      active = false;
      const connected = trellis;
      trellis = null;
      providerState = "connecting";
      if (connected) {
        void connected.connection.close();
      }
    };
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
