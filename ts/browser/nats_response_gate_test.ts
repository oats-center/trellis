import assert from "node:assert/strict";
import { test } from "node:test";

import {
  encodeNatsMsg,
  encodeNatsPub,
  NatsProtocolParser,
  NatsResponseGate,
} from "./nats_response_gate.ts";

test("NATS parser splits records across chunks including CRLF in payload", () => {
  const payload = new TextEncoder().encode("hello\r\nworld");
  const record = encodeNatsPub("rpc.Echo", payload, "_INBOX.a");
  const parser = new NatsProtocolParser();
  parser.push(record.slice(0, 7));
  assert.deepEqual(parser.takeRecords(), []);
  parser.push(record.slice(7));
  const records = parser.takeRecords();
  assert.deepEqual(records.length, 1);
  assert.deepEqual(records[0].op, "PUB");
  assert.deepEqual(records[0].subject, "rpc.Echo");
  assert.deepEqual(records[0].reply, "_INBOX.a");
  assert.deepEqual(
    new TextDecoder().decode(records[0].payload),
    "hello\r\nworld",
  );
});

test("NATS parser reads multiple records from one chunk", () => {
  const ping = new TextEncoder().encode("PING\r\nPONG\r\n");
  const parser = new NatsProtocolParser();
  parser.push(ping);
  const records = parser.takeRecords();
  assert.deepEqual(
    records.map((record) => record.op),
    ["PING", "PONG"],
  );
});

test("response gate holds only the armed reply and forwards PING", async () => {
  const gate = new NatsResponseGate();
  const socket = gate.connect();
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
  assert.deepEqual(socket.ingestClient(pub).length, 1);
  assert.deepEqual(socket.ingestServer(ping).length, 1);
  assert.deepEqual(socket.ingestServer(other).length, 1);
  assert.deepEqual(socket.ingestServer(reply).length, 0);
  const meta = await held;
  assert.deepEqual(meta.reply, "_INBOX.q1");
  assert.deepEqual(meta.route, "events.v1.query");
  const released = gate.release();
  assert.deepEqual(released.length, 1);
  gate.dispose();
});

test("response gate forwards late replies from before arming and holds the next request", async () => {
  const gate = new NatsResponseGate();
  const socket = gate.connect();
  const payload = new Uint8Array(0);
  socket.ingestClient(encodeNatsPub("route.A", payload, "_INBOX.old"));
  const held = gate.armNextReply("route.A");
  socket.ingestClient(encodeNatsPub("route.A", payload, "_INBOX.next"));
  const old = encodeNatsMsg("_INBOX.old", "1", payload);
  const next = encodeNatsMsg("_INBOX.next", "1", payload);
  try {
    assert.deepEqual(socket.ingestServer(old), [old]);
    assert.deepEqual(socket.ingestServer(next), []);
    assert.equal((await held).reply, "_INBOX.next");
    assert.deepEqual(gate.release(), [next]);
  } finally {
    gate.dispose();
  }
});

test("response gate observes a server push that is not an RPC reply", async () => {
  const gate = new NatsResponseGate();
  const socket = gate.connect();
  const push = gate.armNextPush();
  const frame = encodeNatsMsg(
    "deliver.events",
    "9",
    new TextEncoder().encode('{"kind":"event"}'),
  );
  assert.deepEqual(socket.ingestServer(frame).length, 1);
  const record = await push;
  assert.deepEqual(record.subject, "deliver.events");
  gate.dispose();
});

test("response gate does not report a correlated RPC reply as a push", async () => {
  const gate = new NatsResponseGate();
  const socket = gate.connect();
  const subjects: string[] = [];
  void gate.armNextPush().then((record) => subjects.push(record.subject ?? ""));
  assert.deepEqual(
    socket.ingestClient(encodeNatsPub("route.A", new Uint8Array(0), "_INBOX.q"))
      .length,
    1,
  );
  const reply = encodeNatsMsg("_INBOX.q", "1", new Uint8Array(0));
  assert.deepEqual(socket.ingestServer(reply).length, 1);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(subjects, []);
  gate.dispose();
});

test("response gate starts a new socket clean after the last one closed mid-record", async () => {
  // The broker cuts large replies into 4 KiB WebSocket frames, and a page
  // navigation can close its socket between two frames of one record.
  const gate = new NatsResponseGate();
  try {
    const first = gate.connect();
    const held = gate.armNextReply("route.A");
    const large = encodeNatsMsg("_INBOX.other", "1", new Uint8Array(8192));
    assert.deepEqual(first.ingestServer(large.slice(0, 4096)), []);

    const second = gate.connect();
    const info = new TextEncoder().encode('INFO {"server_id":"x"}\r\n');
    assert.deepEqual(second.ingestServer(info), [info]);
    // Holds still span sockets: a reply on the new socket to the route armed
    // before the reconnect is held.
    second.ingestClient(
      encodeNatsPub("route.A", new Uint8Array(0), "_INBOX.b"),
    );
    const reply = encodeNatsMsg("_INBOX.b", "1", new Uint8Array(0));
    assert.deepEqual(second.ingestServer(reply), []);
    assert.equal((await held).reply, "_INBOX.b");
  } finally {
    gate.dispose();
  }
});
