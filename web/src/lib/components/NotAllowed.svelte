<script lang="ts">
  import { getChangeAccess } from "../console/change_access.ts";
  import Icon from "./Icon.svelte";

  type Props = {
    /** What the user would see here, e.g. "users". */
    what?: string;
    class?: string;
  };

  let { what, class: className = "" }: Props = $props();
  const changeAccess = getChangeAccess();
  let busy = $state(false);
</script>

<div
  class={[
    "flex flex-col items-center rounded-box border border-base-300 bg-base-100 px-6 py-12 text-center",
    className,
  ]}
>
  <span
    class="grid h-11 w-11 place-items-center rounded-full bg-base-200 text-base-content/60"
    ><Icon name="lock" size={20} /></span
  >
  <h2 class="mt-4 text-base font-semibold">Not allowed yet</h2>
  <p class="mt-1 max-w-sm text-sm text-base-content/65">
    {what
      ? `You haven't allowed the Console to see ${what}.`
      : "You haven't allowed the Console to do this."}
  </p>
  <button
    class="btn btn-primary btn-sm mt-5"
    disabled={busy}
    onclick={() => {
      busy = true;
      void changeAccess().finally(() => (busy = false));
    }}>Change access</button
  >
</div>
