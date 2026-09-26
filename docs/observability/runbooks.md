# Trellis observability runbooks

These runbooks are operational guidance for the reference alerts. They do not authorize the implementation agent to change a production instance, account, grant, queue, retention policy, or notification destination. Start with **freshness and ownership** before trusting a gauge. Browser reports are diagnostic and untrusted; use native metrics, authoritative runtime state, and existing Console/CLI views for decisions.

## Component unavailable

**Signal:** an expected platform/Jobs/Events/Health component is absent, stale, or not reporting the existing healthy owner/task state for two minutes.

Check the expected-component inventory against the actual selected runtime modes. Then check Prometheus scrape health, Collector self-metrics and the component's last observation time. An HTTP `/readyz` response only confirms that the current endpoint serves version metadata; it does not prove usable authorization or queues. Check process/supervisor logs, singleton ownership, NATS health and local storage before restarting one failed owner. Do not create two active owners or bypass the singleton lease. Confirm that a recovered component advances observation time and has current ownership.

## Snapshot stale

**Signal:** a component process can still report, but one read-only telemetry snapshot has not completed recently.

Inspect `trellis.snapshot.errors`, SQL wait/execute spans, broker requests and Collector freshness. A successful scrape of an old snapshot is not a successful sampler. Pending and age values are last known, not current zero. Check for one stuck query rather than increasing poll concurrency. Restore the dependency and verify timestamp advancement; do not clear queues to make counts smaller.

## RPC budget

**Signal:** sustained infrastructure error fraction exceeds the reference burn threshold and minimum traffic floor.

Compare client logical calls, actual transport attempts and server outcomes. A high retry count with few server spans suggests routing/transport; a server `error`/`timeout` suggests handler/storage/dependency work. Declared errors and authorization denials are separate and must not be fixed by broadly granting privileges. Use a failing trace's stable route and request correlation to inspect the existing operation. Check deployment/provider availability and recent changes. Roll back or correct a concrete cause; do not change timeout/error classification merely to stop the alert.

## Auth refresh

**Signal:** sustained machine-side refresh p95 exceeds the initial one-second budget at meaningful request volume.

Compare context issuance, binding reads, cache hit/miss/wait, semantic CPU queue, SQL wait and callout phases. Verify that the usable-connection boundary still includes admitted transport and own-context coverage. Repeated refresh attempts may indicate a lifecycle defect rather than slow signatures. Never disable revocation coverage, reuse expired authority, or report connected earlier to reduce the histogram. Metrics are an operating budget, not a performance CI threshold.

## Auth delivery

**Signal:** committed Auth post-commit work is older than two minutes and still outstanding.

Treat this as potentially delayed revocation/provisioning delivery. Inspect the existing Auth outbox's oldest action, attempts, predecessor and last error; correlate its action ID in a trace/log without putting it in metrics. Distinguish persisted context revocation from mirror/KICK completion. Check NATS leader/consumer permissions and the exact physical target response. Do not drop the action, delete context history, expand ACLs, regrant an account, or mark an unconfirmed KICK as success. Completion must advance the real outbox, then the metric should fall naturally.

## Jobs backlog

**Signal:** Pending jobs satisfying the documented ready predicate have aged beyond the starting queue budget.

Inspect Jobs workbench by type/service, actual worker registrations, keyed concurrency and dependency waits. `waiting_retry` is separate: a Retry row is not assumed ready without an eligible timestamp. Look at attempt outcomes and queue-policy metadata to distinguish normal coalescing/serial work from worker loss. Do not increase concurrency, change retry policy or clear queued jobs without understanding business side effects. A longer intentional budget belongs in deployment alert policy, not in misleading instrumentation.

## Jobs workers

**Signal:** Pending ready work exists but the current Jobs snapshot contains no fresh worker registrations.

Check service/device connection state and registration freshness under the existing Jobs heartbeat TTL. Confirm matching deployments and that workers are actually mounted; one HTTP process does not prove a worker. Check NATS access, startup failures and lease conflicts. Restart only the affected worker after preserving evidence. No new coordinator, fake heartbeat, or manual database freshness update.

## Consumer stalled

**Signal:** outstanding Consumer work exists with no observed ACK-floor progress for the configured interval.

Inspect Consumers view for backlog, ACK pending, handlers, delivery reports and retry state. ACK pending may be a long handler rather than no workers; compare process duration and current handler activity. Check local concurrency, event verification evidence availability and report publication. Do not ACK, terminate, or replay automatically to make the lag disappear. The metric measures observed progress, not the age of a particular raw stream sequence.

## Consumer missing

**Signal:** an authority-declared durable Consumer is genuinely NotFound at the broker.

Check installed resource binding, provisioning state and owner before any recreation. An authorization or I/O error is not NotFound and must instead make the sampler stale. Use the existing provisioning mechanism; don't manually create an unscoped consumer or broaden watcher permissions. Confirm resource identity/limits and preserved delivery semantics after repair.

## Dead letters

**Signal:** unresolved Event dead letters or undismissed dead Jobs persist.

Use the existing Events DLQ or Jobs interface to inspect cause, original payload under authorized access, and retry/replay history. Fix handler or entitlement defects before targeted replay. Event replay must target the selected Consumer and retain original proof/evidence; it is not broadcast republication. Do not bulk-dismiss, erase history or repeatedly replay an irreversible side effect. Counts are current projected states and must be checked for freshness.

## Projection stalled

**Signal:** a projector has actual broker backlog but no observed checkpoint/ACK progress.

Check the owner's task, SQL/storage health, decode/verification errors and poison-message handling. Compare source Consumer pending/ACK pending with durable projection checkpoint; do not call sequence subtraction a message count. An idle zero-backlog projector should not alert. Rebuild only through the existing supported mechanism after identifying the cause; don't advance checkpoints by hand.

## Connection suspended

**Signal:** native connections remain suspended rather than recovering, or suspend without an unexpected transport loss.

A routine proactive authorization refresh must not suspend a connection: the refreshed credential rotates the physical NATS attachment while the logical connection stays usable, so a suspended connection indicates a rejected credential, lost coverage, or a real transport failure. Inspect own-context coverage, current/candidate digest and admitted epoch in sanitized traces/logs. Distinguish a rejected credential, a valid credential with revoked predecessor, broken watch coverage, ordinary restart and a changed resource generation. Do not force the local usable flag or revive retired handles. Repair the owning connection lifecycle or dependency; use the existing retry/terminal classification.

## Route overflow

**Signal:** the bounded descriptor-route catalog exceeded capacity.

Check whether a developer accidentally registered concrete subjects, resource IDs or user input. Correct the instrumentation source before raising a limit. `_other` aggregation preserves total observations but loses route detail. SDK overflow also loses original attributes, so filtered SLO series may undercount. Do not solve it by hashing IDs or creating one label per principal.

## Collector

**Signal:** native metrics scrape/self-metrics are down or trace export queue is saturated.

Check Collector process, listener binding, TLS/auth, backend availability, queue capacity and spool disk. The application may still be healthy; missing telemetry is not proof of healthy or unhealthy business work. A full queue can lose observations even after recovery. Keep bounded queues and retry budgets; don't enlarge them indefinitely. Do not restart Trellis to fix a Collector-only outage unless evidence shows a separate application problem. Verify both metrics and traces after recovery; an HTTP success on one listener does not certify both signals.

## HTTP reachability

**Signal:** the configured `/readyz` probe does not return HTTP success.

Check network, reverse proxy, listener and process. The current endpoint returns version metadata, so success is only reachability/liveness evidence. Use component state and real traffic metrics to diagnose deeper readiness. Do not weaken auth or add an unauthenticated admin probe.

## NATS

**Signal:** the private NATS health probe fails or aggregate broker metrics show capacity/replication trouble.

Check server health, JetStream storage, cluster leaders, memory/disk and application connections. Keep monitoring private; never expose unauthenticated monitoring ports publicly. Do not enable per-client/per-subject metric dumps as a default fix. Avoid simultaneous restarts of all brokers/owners; preserve quorum and storage.

## Host capacity

**Signal:** sustained low disk or available memory.

Identify the actual filesystem and process. Check Trellis/NATS data retention, SQL WAL, Collector trace spool and export backlog separately. Never delete NATS/SQL/Auth context files manually to reclaim space. Follow supported retention/maintenance or add capacity. Confirm that monitoring scrapes the host namespace/data mounts rather than a container's unrelated root filesystem.

## Clock

**Signal:** host clock offset exceeds the reference threshold.

Check NTP/time synchronization and VM/host clocks. Clock error affects wall-clock ages and authorization validity; don't compensate by widening proof windows or granting longer contexts. Metrics measured with monotonic time remain durations, but persisted age/freshness interpretation requires synchronized clocks.
