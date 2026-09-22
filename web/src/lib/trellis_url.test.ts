import { assertEquals, assertThrows } from "@std/assert";
import { resolveTrellisUrl } from "./trellis_url.ts";

Deno.test("resolveTrellisUrl prefers explicit public URL", () => {
  assertEquals(
    resolveTrellisUrl(" https://auth.example.com/ ", "https://served.example"),
    "https://auth.example.com",
  );
});

Deno.test("resolveTrellisUrl falls back to browser origin", () => {
  assertEquals(
    resolveTrellisUrl(undefined, "http://krishi.trellis.qlever.io"),
    "http://krishi.trellis.qlever.io",
  );
});

Deno.test("resolveTrellisUrl keeps localhost fallback for non-browser tooling", () => {
  assertEquals(
    resolveTrellisUrl(undefined, undefined),
    "http://localhost:3000",
  );
});

Deno.test("resolveTrellisUrl validates explicit public URL", () => {
  assertThrows(
    () => resolveTrellisUrl("not a url", "https://served.example"),
    Error,
    "Invalid PUBLIC_TRELLIS_URL",
  );
});
