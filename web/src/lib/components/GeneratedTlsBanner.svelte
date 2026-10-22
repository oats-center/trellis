<script lang="ts">
  import Notice from "./Notice.svelte";
  import { generatedTlsNotice } from "../config";

  /** Days before leaf expiry at which the notice escalates to an error. */
  const EXPIRY_WARNING_DAYS = 14;

  const notice = generatedTlsNotice();
  const expires = notice?.leafNotAfter ? new Date(notice.leafNotAfter) : null;
  const expiringSoon =
    expires !== null &&
    expires.getTime() - Date.now() < EXPIRY_WARNING_DAYS * 24 * 60 * 60 * 1000;
</script>

{#if notice}
  <Notice
    variant={expiringSoon ? "error" : "warning"}
    role="status"
    class="rounded-none text-sm"
  >
    This Trellis uses a private certificate authority generated at installation.
    {#if notice.caSha256Fingerprint}
      Trust it only if this SHA-256 fingerprint matches the one your operator
      published:
      <code class="font-mono break-all">{notice.caSha256Fingerprint}</code>.
    {:else}
      Its certificates could not be read; contact your operator.
    {/if}
    {#if expires}
      The server certificate {expiringSoon ? "expires soon" : "expires"} on
      {expires.toLocaleString()}.
    {/if}
  </Notice>
{/if}
