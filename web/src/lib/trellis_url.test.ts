import { deepEqual, throws } from "node:assert/strict";
import { test } from "node:test";
import { resolveTrellisUrl } from "./trellis_url.ts";

test("resolveTrellisUrl prefers explicit public URL", () => {
  deepEqual(
    resolveTrellisUrl(" https://auth.example.com/ ", "https://served.example"),
    "https://auth.example.com",
  );
});

test("resolveTrellisUrl falls back to browser origin", () => {
  deepEqual(
    resolveTrellisUrl(undefined, "http://trellis.example.com"),
    "http://trellis.example.com",
  );
});

test("resolveTrellisUrl validates explicit public URL", () => {
  throws(
    () => resolveTrellisUrl("not a url", "https://served.example"),
    /Invalid PUBLIC_TRELLIS_URL/,
  );
});
