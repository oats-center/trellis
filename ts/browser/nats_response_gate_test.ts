import { assertEquals } from "@std/assert";

import {
  encodeNatsMsg,
  encodeNatsPub,
  NatsProtocolParser,
  NatsResponseGate,
} from "./nats_response_gate.ts";

Deno.test("NATS parser splits records across chunks including CRLF in payload", () => {
  const payload = new TextEncoder().encode("hello\r\nworld");
  const record = encodeNatsPub("rpc.Echo", payload, "_INBOX.a");
  const parser = new NatsProtocolParser();
  parser.push(record.slice(0, 7));
  assertEquals(parser.takeRecords(), []);
  parser.push(record.slice(7));
  const records = parser.takeRecords();
  assertEquals(records.length, 1);
  assertEquals(records[0].op, "PUB");
  assertEquals(records[0].subject, "rpc.Echo");
  assertEquals(records[0].reply, "_INBOX.a");
  assertEquals(new TextDecoder().decode(records[0].payload), "hello\r\nworld");
});

Deno.test("NATS parser reads multiple records from one chunk", () => {
  const ping = new TextEncoder().encode("PING\r\nPONG\r\n");
  const parser = new NatsProtocolParser();
  parser.push(ping);
  const records = parser.takeRecords();
  assertEquals(records.map((record) => record.op), ["PING", "PONG"]);
});

Deno.test("response gate holds only the armed reply and forwards PING", async () => {
  const gate = new NatsResponseGate();
  const held = gate.armNextReply("events.v1.query");
  const pub = encodeNatsPub(
    "events.v1.query",
    new TextEncoder().encode("{}"),
    "_INBOX.q1",
  );
  const ping = new TextEncoder().encode("PING\r\n");
  const reply = encodeNatsMsg(
    "_INBOX.q1",
    "1",
    new TextEncoder().encode('{"ok":true}'),
  );
  const other = encodeNatsMsg(
    "_INBOX.other",
    "1",
    new TextEncoder().encode("nope"),
  );
  assertEquals(gate.ingestClient(pub).length, 1);
  assertEquals(gate.ingestServer(ping).length, 1);
  assertEquals(gate.ingestServer(other).length, 1);
  assertEquals(gate.ingestServer(reply).length, 0);
  const meta = await held;
  assertEquals(meta.reply, "_INBOX.q1");
  assertEquals(meta.route, "events.v1.query");
  const released = gate.release();
  assertEquals(released.length, 1);
  gate.dispose();
});

Deno.test("response gate observes a server push that is not an RPC reply", async () => {
  const gate = new NatsResponseGate();
  const push = gate.armNextPush();
  const frame = encodeNatsMsg(
    "deliver.events",
    "9",
    new TextEncoder().encode('{"kind":"event"}'),
  );
  assertEquals(gate.ingestServer(frame).length, 1);
  const record = await push;
  assertEquals(record.subject, "deliver.events");
  gate.dispose();
});

Deno.test("response gate does not report a correlated RPC reply as a push", async () => {
  const gate = new NatsResponseGate();
  const subjects: string[] = [];
  void gate.armNextPush().then((record) => subjects.push(record.subject ?? ""));
  assertEquals(
    gate.ingestClient(
      encodeNatsPub("route.A", new Uint8Array(0), "_INBOX.q"),
    ).length,
    1,
  );
  const reply = encodeNatsMsg("_INBOX.q", "1", new Uint8Array(0));
  assertEquals(gate.ingestServer(reply).length, 1);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assertEquals(subjects, []);
  gate.dispose();
});
