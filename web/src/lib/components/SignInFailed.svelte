<script lang="ts">
  import { isErr } from "@oatscenter/result";
  import { TrellisClient } from "@oatscenter/trellis";
  import { APP_CONFIG, consoleAuthOptions } from "$lib/config";
  import {
    consoleReturnKey,
    resetAutomaticSignIn,
    type SignInProblem,
  } from "$lib/auth";
  import { connectionProblem } from "$lib/connection_problem";
  import TrellisLogo from "./TrellisLogo.svelte";

  type Props = { problem: SignInProblem };
  let { problem }: Props = $props();

  let restarting = $state(false);
  let restartProblem = $state("");

  // Retrying with the same app session and Portal login would fail the same
  // way, so starting over ends both and asks who is signing in.
  async function restart(): Promise<void> {
    restarting = true;
    resetAutomaticSignIn();
    sessionStorage.removeItem(consoleReturnKey());
    const ended = (
      await TrellisClient.signOut({
        trellisUrl: APP_CONFIG.authUrl,
        auth: consoleAuthOptions(),
        returnTo: APP_CONFIG.callbackUri,
      })
    ).take();
    if (isErr(ended)) {
      restartProblem = connectionProblem(ended.error);
      restarting = false;
    }
  }
</script>

<main class="portal-shell flex min-h-screen justify-center px-4 py-10 sm:px-6">
  <section class="w-full max-w-xl space-y-6">
    <TrellisLogo />
    <div role="alert" class="space-y-2">
      <p class="font-semibold">Sign-in could not be completed.</p>
      <p class="portal-copy">{problem.summary}</p>
      {#if problem.reference}
        <p class="portal-copy text-sm">
          Reference: <span class="mono">{problem.reference}</span>
        </p>
      {/if}
    </div>
    {#if problem.detail}
      <details class="portal-copy text-sm">
        <summary class="cursor-pointer">Details</summary>
        <p class="mono mt-2">{problem.detail}</p>
      </details>
    {/if}
    {#if restartProblem}<p class="portal-copy text-sm">{restartProblem}</p>{/if}
    <button class="btn btn-primary" disabled={restarting} onclick={restart}
      >Sign in again</button
    >
  </section>
</main>
