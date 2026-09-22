import { equal } from "node:assert/strict";
import { subjectMatches } from "./subject.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

Deno.test("subjectMatches implements NATS token wildcards", () => {
  equal(subjectMatches("events.*.Opened", "events.Connections.Opened"), true);
  equal(
    subjectMatches("events.*.Opened", "events.Connections.Admin.Opened"),
    false,
  );
  equal(subjectMatches("events.>", "events.Connections.Opened"), true);
  equal(subjectMatches("events.>", "events"), false);
  equal(
    subjectMatches("events.Connections", "events.Connections.Opened"),
    false,
  );
});
