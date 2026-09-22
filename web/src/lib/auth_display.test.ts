import { deepEqual, equal } from "node:assert/strict";

import {
  describeSessionPrincipal,
  describeUserGrant,
  formatShortKey,
  participantKindLabel,
} from "./auth_display.ts";

Deno.test("describeSessionPrincipal renders generated session identity", () => {
  deepEqual(
    describeSessionPrincipal({
      createdAt: 1n,
      expiresAt: null,
      lastAuthenticatedAt: 2n,
      participantId: "console",
      participantKind: "app",
      principalId: "usr_123",
      revokedAt: null,
      sessionId: "ses_app",
      sessionKeyId: "key_app",
      sessionPublicKey: "pub_app",
      state: "active",
      version: 1n,
    }),
    {
      title: "usr_123",
      details: "ses_app",
    },
  );
});

Deno.test("grant helpers expose honest participant labels and compact keys", () => {
  equal(participantKindLabel("app"), "App");
  equal(participantKindLabel("agent"), "Agent");
  equal(participantKindLabel("device"), "Device");
  equal(participantKindLabel("service"), "Service");
  equal(formatShortKey("abcdefghijklmnopqrstuvwxyz", 8), "abcdefgh…");
  equal(formatShortKey(undefined), "—");
  deepEqual(
    describeUserGrant({
      identityGrantId: "agent-grant",
      contractEvidence: {
        contractDigest: "digest-agent",
        contractId: "trellis.agent@v1",
      },
      displayName: "Trellis Agent",
      description: "Local delegated tooling",
      participantKind: "agent",
      capabilities: ["jobs.read"],
      grantedAt: "2026-04-10T00:00:00.000Z",
      updatedAt: "2026-04-11T00:00:00.000Z",
    }),
    {
      title: "Trellis Agent",
      details: "Agent grant • trellis.agent@v1",
    },
  );
});

declare const Deno: {
  test(name: string, fn: () => void | Promise<void>): void;
};
