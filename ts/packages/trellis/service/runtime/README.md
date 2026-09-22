# Trellis Service Source Package

Internal runtime implementation for TypeScript Trellis services.

- `@qlever-llc/trellis/service` — shared service core, types, and service
  helpers
- `@qlever-llc/trellis/service` — shared Node-compatible native service runtime

Use the runtime-specific subpath when connecting a service.

Resolved service resource bindings are runtime internals. Service authors should
connect with `TrellisService.connect(...)` and use the returned `service.kv`,
`service.store`, and `service.jobs` handles; do not call `Trellis.Bindings.Get`,
construct `TrellisService` or `StoreHandle`, or pass binding/resource payloads
into `Trellis` constructors.

Connected services keep provider registration under `service.handle`, typed from
the owned contract surface.

Handlers receive a scoped `client` in their context for outbound calls through
generated facades such as `client.rpc.<group>.<leaf>(...)`,
`client.event.<group>.<leaf>.publish(...)`, and
`client.operation.<group>.<leaf>.start(...)`. Owned service surfaces register as
`service.handle.rpc.<group>.<leaf>(handler)`,
`service.handle.feed.<group>.<leaf>(handler)`, and
`service.handle.operation.<group>.<leaf>(provider)`.

Durable event delivery is built around prepared events. Use
`client.event.<group>.<leaf>.prepare(event)` to create a `PreparedTrellisEvent`,
then persist it with `SqlOutboxRepository` or `NatsKvOutboxRepository` and flush
it with `dispatchOutbox` or a process-local `OutboxDispatcher`. When using a
transactional outbox, call `OutboxDispatcher.notify()` only after commit.
`SqlInboxRepository` and `NatsKvInboxRepository` provide idempotent inbox
tracking for consumers.

Durable event consumption is built around contract-declared `eventConsumers`.
The connected service receives Trellis-provisioned bindings during bootstrap;
handler code listens with
`client.event.<group>.<leaf>.listen(handler, data,
{ group: "consumerGroup" })`.
Service code must not create or name JetStream durable consumers directly.
Ephemeral listeners remain available for live-only processing with
`{ mode: "ephemeral", replay: "new" }`.
