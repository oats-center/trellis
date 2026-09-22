<script module lang="ts">
  export type ConfirmationRequest = {
    title: string;
    message: string;
    confirmLabel: string;
    targetLabel?: string;
    targetName?: string;
    expectedValue?: string;
    details?: string;
  };
</script>

<script lang="ts">
  import { onDestroy, tick } from "svelte";

  let dialog: HTMLDialogElement | undefined = $state();
  let request = $state<ConfirmationRequest | null>(null);
  let typedValue = $state("");
  let resolver: ((confirmed: boolean) => void) | null = null;
  // Monotonic confirmation generation: a teardown or a newer confirmation can
  // only resolve the request it owns, never a replacement's resolver.
  let generation = 0;

  const requiredValue = $derived(request?.expectedValue?.trim() ?? "");
  const canConfirm = $derived(!requiredValue || typedValue.trim() === requiredValue);

  function resolvePending(confirmed: boolean): void {
    const resolve = resolver;
    resolver = null;
    generation += 1;
    resolve?.(confirmed);
  }

  /**
   * Opens a destructive-action confirmation modal and resolves with the operator's decision.
   */
  export async function confirm(nextRequest: ConfirmationRequest): Promise<boolean> {
    // Supersede any outstanding confirmation before opening the next one.
    resolvePending(false);
    if (dialog?.open) dialog.close();

    const owned = ++generation;
    request = nextRequest;
    typedValue = "";
    await tick();

    if (!dialog || owned !== generation) return false;
    dialog.showModal();

    return new Promise<boolean>((resolve) => {
      if (owned !== generation) {
        resolve(false);
        return;
      }
      resolver = resolve;
    });
  }

  /** Cancels an outstanding confirmation when its target or authority changes. */
  export function cancel(): void {
    if (resolver || request) finish(false);
  }

  function finish(confirmed: boolean) {
    if (confirmed && !canConfirm) return;
    request = null;
    typedValue = "";
    if (dialog?.open) dialog.close();
    resolvePending(confirmed);
  }

  function handleCancel(event: Event) {
    event.preventDefault();
    finish(false);
  }

  function handleClose() {
    if (resolver) finish(false);
  }

  onDestroy(() => {
    // Teardown must resolve, not strand, an awaited confirmation.
    resolvePending(false);
  });
</script>

<dialog bind:this={dialog} class="modal modal-bottom sm:modal-middle" oncancel={handleCancel} onclose={handleClose}>
  {#if request}
    <div class="modal-box border border-error/30">
      <h3 class="text-lg font-semibold">{request.title}</h3>
      <p class="mt-2 text-sm text-base-content/70">{request.message}</p>

      {#if request.details}
        <div class="mt-3 rounded-box border border-base-300 bg-base-200/50 p-3 text-xs text-base-content/70">
          {request.details}
        </div>
      {/if}

      {#if request.targetName}
        <div class="mt-3 rounded-box border border-error/25 bg-error/10 p-3 text-sm">
          <div class="text-[0.65rem] font-semibold uppercase tracking-wide text-base-content/55">{request.targetLabel ?? "Target"}</div>
          <div class="trellis-identifier mt-1 break-all font-medium">{request.targetName}</div>
        </div>
      {/if}

      {#if requiredValue}
        <label class="form-control mt-4 gap-1">
          <span class="label-text text-xs">Type <span class="trellis-identifier font-semibold">{requiredValue}</span> to confirm</span>
          <input class="input input-bordered input-sm" bind:value={typedValue} autocomplete="off" />
        </label>
      {/if}

      <div class="modal-action">
        <button type="button" class="btn btn-ghost btn-sm" onclick={() => finish(false)}>Cancel</button>
        <button type="button" class="btn btn-error btn-sm" disabled={!canConfirm} onclick={() => finish(true)}>{request.confirmLabel}</button>
      </div>
    </div>
    <form method="dialog" class="modal-backdrop">
      <button>Cancel</button>
    </form>
  {/if}
</dialog>
