import { assertEquals } from "@std/assert";
import { portalIntentFromUrl, portalTransactionIdFromUrl } from "./portal.ts";

Deno.test("portal navigation transports an opaque intent separately from an active provider attempt", () => {
  const url = new URL("https://portal.example.com/login");
  url.searchParams.set("intent", "signed.+/request&correlation");
  assertEquals(
    portalIntentFromUrl(new URL(url.href)),
    "signed.+/request&correlation",
  );
  const callback = new URL(
    "https://app.example.com/?transactionId=attempt%2F1",
  );
  assertEquals(
    portalTransactionIdFromUrl(callback),
    "attempt/1",
  );
});
