import { assertEquals, assertStrictEquals } from "@std/assert";
import { apiDescriptor, codecs, participantDescriptor } from "../generated.ts";
import { eventSubjectWasm } from "../auth/protocol_wasm.ts";
import {
  boundApiSubject,
  eventSubject,
  feedControlSubject,
  routeQueueGroup,
} from "./api.ts";
import {
  bindApiRoutes,
  type GeneratedParticipant,
  getParticipantRuntime,
  participantAvailability,
  participantEvidence,
  refreshApiRoutes,
} from "./participant.ts";

Deno.test("deployment subjects and replica queues match protocol vectors", () => {
  const subject = boundApiSubject(
    "operation",
    "acme-orders.orders@v1",
    "deployment-01",
    "Refund.Start",
  );
  assertEquals(
    subject,
    "operation.v1.YWNtZS1vcmRlcnMub3JkZXJzQHYx.ZGVwbG95bWVudC0wMQ.Refund.Start",
  );
  assertEquals(
    routeQueueGroup(subject),
    "trellis.XzNEqEQ-aqJzGNmO3z42LIzwJkDJnEgfRQIylDrYUUQ",
  );
  assertEquals(
    feedControlSubject(
      boundApiSubject(
        "feed",
        "acme-orders.orders@v1",
        "deployment-01",
        "Watch",
      ),
      "instance-01",
      "feed-01",
    ),
    "feed.v1.YWNtZS1vcmRlcnMub3JkZXJzQHYx.ZGVwbG95bWVudC0wMQ.Watch.control.aW5zdGFuY2UtMDE.ZmVlZC0wMQ",
  );
});

Deno.test("event subjects include the qualified API identity", () => {
  assertEquals(
    eventSubject("acme-orders.runtime@v1", "Changed"),
    "events.v1.YWNtZS1vcmRlcnMucnVudGltZUB2MQ.Changed",
  );
  assertEquals(
    eventSubject("other.runtime@v1", "Changed"),
    "events.v1.b3RoZXIucnVudGltZUB2MQ.Changed",
  );
});

Deno.test("WASM event subject matches the TypeScript runtime", async () => {
  const apiId = "acme-orders.runtime@v1";
  assertEquals(
    await eventSubjectWasm(apiId, "Changed"),
    eventSubject(apiId, "Changed"),
  );
});

Deno.test("optional State availability follows exact resource grants", () => {
  const participant: GeneratedParticipant = {
    kind: "app",
    id: "example.Console",
    identity: "example.Console",
    path: "Console",
    actionNames: {},
    implements: [],
    uses: [],
    resources: {
      counter: {
        kind: "state",
        availability: "optional",
        codec: codecs.u64,
        version: 1,
        migrations: {},
      },
    },
    packageEvidence: {
      rootPackage: "example",
      rootDigest: "digest",
      packages: [],
    },
  };
  assertEquals(participantAvailability(participant, {}, {}, []).resources, {
    counter: false,
  });
  assertEquals(
    participantAvailability(participant, {}, {}, [{
      target: {
        kind: "participantResource",
        participant: "example.Console",
        resource: "state",
        name: "counter",
      },
    }]).resources,
    { counter: true },
  );
});

Deno.test("generated participant descriptors project into the runtime", () => {
  const api = apiDescriptor({
    identity: "example.orders@v1",
    actions: {
      "rpc:Get": {
        kind: "rpc",
        descriptorName: "rpc:Get",
        input: codecs.string,
        output: codecs.u64,
        errors: [],
        download: false,
        pagination: undefined,
      },
      "event:Changed": {
        kind: "event",
        descriptorName: "event:Changed",
        payload: codecs.bytes,
        parameters: [["orderId"]],
      },
    },
  });
  const packageEvidence = {
    rootPackage: "example",
    rootDigest: "package-digest",
    packages: [{
      name: "example",
      version: "1.0.0",
      digest: "package-digest",
      source: "package example@1.0.0;",
    }],
  };
  const participant = participantDescriptor({
    kind: "app",
    id: "example.Console",
    identity: "example.Console",
    path: "Console",
    actionNames: {
      "example.orders@v1:rpc:Get": "Get",
      "example.orders@v1:event:Changed": "Changed",
    },
    implements: [],
    uses: [{
      api,
      actions: [
        { descriptorName: "rpc:Get", direction: "call" },
        { descriptorName: "event:Changed", direction: "subscribe" },
      ],
      optionalCapabilities: [],
    }],
    resources: {},
    packageEvidence,
  });

  const runtime = getParticipantRuntime(participant);
  assertStrictEquals(runtime.usedApi.rpc.Get.input, codecs.string);
  assertEquals(runtime.usedApi.rpc.Get.subject, "rpc.v1.orders.Get");
  assertEquals(runtime.usedApi.rpc.Get.permission, {
    apiId: "example.orders",
    apiVersion: "v1",
    surfaceKind: "rpc",
    surfaceName: "Get",
    action: "call",
  });
  assertEquals(
    runtime.usedApi.events.Changed.subject,
    "events.v1.ZXhhbXBsZS5vcmRlcnNAdjE.Changed.{/orderId}",
  );
  assertEquals(runtime.actions.map((action) => action.connectedName), [
    "get",
    "onChanged",
  ]);
  const bound = bindApiRoutes(runtime.usedApi, {
    "example.orders@v1": { providerDeploymentId: "deployment-01" },
  });
  const descriptor = bound.rpc.Get;
  assertEquals(
    descriptor.subject,
    boundApiSubject("rpc", "example.orders@v1", "deployment-01", "Get"),
  );
  refreshApiRoutes(bound, {
    "example.orders@v1": { providerDeploymentId: "deployment-02" },
  });
  assertStrictEquals(bound.rpc.Get, descriptor);
  assertEquals(
    descriptor.subject,
    boundApiSubject("rpc", "example.orders@v1", "deployment-02", "Get"),
  );

  const evidence = participantEvidence(participant);
  assertStrictEquals(evidence.packageEvidence, packageEvidence);
  assertEquals(evidence.participantPath, "Console");
  assertEquals(evidence.packageDigest, "package-digest");
});
