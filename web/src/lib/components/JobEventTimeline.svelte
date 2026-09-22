<script lang="ts">
  import {
    buildJobTimeline,
    type JobTimelineRuntimeEvent,
    type JobTimelineWait,
  } from "../job_event_timeline";

  type Props = {
    events: Parameters<typeof buildJobTimeline>[0];
  };

  let { events }: Props = $props();

  const phases = $derived(buildJobTimeline(events));
  const dateTimeFormat = new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
  });
  const timeFormat = new Intl.DateTimeFormat(undefined, {
    hour: "numeric",
    minute: "2-digit",
    second: "2-digit",
    fractionalSecondDigits: 3,
  });

  function formatTimestamp(value: string, includeDate = false): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;
    return (includeDate ? dateTimeFormat : timeFormat).format(date);
  }

  function outcomeTone(state: string): string {
    if (state === "completed") return "outcome-success";
    if (["failed", "dead", "expired", "stale", "dismissed"].includes(state)) {
      return "outcome-error";
    }
    if (["retry", "cancelled", "skipped"].includes(state)) {
      return "outcome-warning";
    }
    return "outcome-neutral";
  }
</script>

{#snippet dependencyWait(wait: JobTimelineWait)}
  <li class="dependency-wait">
    <div class="dependency-heading">
      <span class="dependency-kind">Dependency wait</span>
      {#if wait.duration}
        <strong class="dependency-duration">{wait.duration}</strong>
      {/if}
    </div>
    <div class="dependency-name break-anywhere">{wait.label}</div>
    {#if wait.detail}
      <div class="dependency-detail break-anywhere">{wait.detail}</div>
    {/if}
    <div class="dependency-timing">
      <span class="event-type break-anywhere">{wait.rawEvents[0].type}</span>
      <time datetime={wait.startedAt} title={wait.startedAt}>{formatTimestamp(wait.startedAt)}</time>
      {#if wait.endTimestamp}
        <span class="event-type break-anywhere"
          >{wait.rawEvents[wait.rawEvents.length - 1].type}</span
        >
        <time datetime={wait.endTimestamp} title={wait.endTimestamp}
          >{formatTimestamp(wait.endTimestamp)}</time
        >
      {/if}
    </div>
  </li>
{/snippet}

{#snippet runtimeEvent(event: JobTimelineRuntimeEvent)}
  <li class="runtime-event">
    <div class="runtime-event-heading">
      <span class="event-type break-anywhere">{event.type}</span>
      <time datetime={event.timestamp} title={event.timestamp}
        >{formatTimestamp(event.timestamp)}</time
      >
    </div>
    {#if event.logs && event.logs.length > 0}
      <ol class="log-entries" aria-label="Recorded log entries">
        {#each event.logs as log, index (`${log.timestamp}-${index}`)}
          <li>
            <span class={["log-level", `log-${log.level}`]}>{log.level}</span>
            <span class="break-anywhere">{log.message}</span>
            <time datetime={log.timestamp} title={log.timestamp}>{formatTimestamp(log.timestamp)}</time>
          </li>
        {/each}
      </ol>
    {:else}
      <strong class="break-anywhere">{event.label}</strong>
    {/if}
    {#if event.detail}
      <p class="runtime-event-detail break-anywhere">{event.detail}</p>
    {/if}
  </li>
{/snippet}

<div class="job-execution-story">
  {#if phases.length === 0}
    <p class="empty-story">No timeline events recorded.</p>
  {:else}
    {#each phases as phase (phase.kind + phase.rawEvents[0].sequence)}
      {#if phase.kind === "queue"}
        <section class="story-phase queue-phase" aria-labelledby={`queue-${phase.rawEvents[0].sequence}`}>
          <header class="phase-heading">
            <h3 id={`queue-${phase.rawEvents[0].sequence}`}>{phase.label}</h3>
            {#if phase.duration}
              <strong class="phase-duration">{phase.duration}</strong>
            {/if}
          </header>

          <div class="queue-events">
            <div class="queue-event">
              <span class="queue-marker" aria-hidden="true"></span>
              <div>
                <strong>{phase.enteredLabel}</strong>
                <div class="evidence-line">
                  <span class="event-type">{phase.enteredType}</span>
                  <time datetime={phase.enteredAt} title={phase.enteredAt}
                    >{formatTimestamp(phase.enteredAt, true)}</time
                  >
                </div>
              </div>
            </div>
            {#if phase.exitedAt && phase.exitedLabel}
              <div class="queue-event">
                <span class="queue-marker queue-marker-end" aria-hidden="true"></span>
                <div>
                  <strong>{phase.exitedLabel}</strong>
                  <div class="evidence-line">
                    {#if phase.exitedType}
                      <span class="event-type">{phase.exitedType}</span>
                    {/if}
                    {#if phase.transition}<span>{phase.transition}</span>{/if}
                    <time datetime={phase.exitedAt} title={phase.exitedAt}
                      >{formatTimestamp(phase.exitedAt, true)}</time
                    >
                  </div>
                </div>
              </div>
            {/if}
          </div>
        </section>
      {:else if phase.kind === "execution"}
        <section class="story-phase execution-phase" aria-labelledby={`execution-${phase.rawEvents[0].sequence}`}>
          <header class="phase-heading">
            <h3 id={`execution-${phase.rawEvents[0].sequence}`}>{phase.label}</h3>
            {#if phase.duration}
              <strong class="phase-duration">{phase.duration}</strong>
            {/if}
          </header>

          <div class="execution-summary">
            <strong>{phase.attempt !== undefined ? `Attempt ${phase.attempt}` : "Running"}</strong>
            <div class="evidence-line">
              <span class="event-type">{phase.startedType}</span>
              {#if phase.transition}<span>{phase.transition}</span>{/if}
              <time datetime={phase.startedAt} title={phase.startedAt}
                >{formatTimestamp(phase.startedAt, true)}</time
              >
            </div>
            {#if phase.workerInstanceId}
              <div class="worker-line break-anywhere">
                Worker <span class="trellis-identifier">{phase.workerInstanceId}</span>
              </div>
            {/if}
          </div>

          {#if phase.steps.length > 0}
            <h4 class="work-heading">Work performed</h4>
            <ol class="work-steps" aria-label="Work performed">
              {#each phase.steps as step, index (step.rawEvents[0].sequence)}
                <li class="work-step">
                  <div class="step-rail" aria-hidden="true">
                    <span>{index + 1}</span>
                  </div>
                  <div class="step-content">
                    <div class="step-heading">
                      <h4 class="break-anywhere">{step.label}</h4>
                      <time datetime={step.timestamp} title={step.timestamp}
                        >{formatTimestamp(step.timestamp)}</time
                      >
                    </div>
                    <div class="step-evidence">
                      {#if step.detail}<span class="break-anywhere">{step.detail}</span>{/if}
                      <span class="event-type">{step.type}</span>
                    </div>
                    {#if step.waits.length > 0}
                      <ol class="dependency-waits" aria-label={`Dependencies while ${step.label}`}>
                        {#each step.waits as wait (wait.rawEvents[0].sequence)}
                          {@render dependencyWait(wait)}
                        {/each}
                      </ol>
                    {/if}
                  </div>
                </li>
              {/each}
            </ol>
          {:else}
            <p class="no-work-updates">No work steps were reported.</p>
          {/if}

          {#if phase.waits.length > 0}
            <div class="execution-evidence-group">
              <h4>Execution dependencies</h4>
              <ol class="dependency-waits execution-waits">
                {#each phase.waits as wait (wait.rawEvents[0].sequence)}
                  {@render dependencyWait(wait)}
                {/each}
              </ol>
            </div>
          {/if}

          {#if phase.events.length > 0}
            <div class="execution-evidence-group">
              <h4>Runtime events</h4>
              <ol class="runtime-events">
                {#each phase.events as event (event.rawEvents[0].sequence)}
                  {@render runtimeEvent(event)}
                {/each}
              </ol>
            </div>
          {/if}
        </section>
      {:else}
        <section
          class={["story-phase", "outcome-phase", outcomeTone(phase.state)]}
          aria-labelledby={`outcome-${phase.rawEvents[0].sequence}`}
        >
          <header class="phase-heading">
            <h3 id={`outcome-${phase.rawEvents[0].sequence}`}>Outcome</h3>
            {#if phase.duration}
              <strong class="phase-duration"
                >{phase.duration} {phase.durationKind === "queue" ? "queued" : "runtime"}</strong
              >
            {/if}
          </header>
          <div class="outcome-content">
            <span class="outcome-marker" aria-hidden="true"></span>
            <div>
              <h4>{phase.label}</h4>
              <div class="evidence-line">
                <span class="event-type">{phase.type}</span>
                {#if phase.transition}<span>{phase.transition}</span>{/if}
                <time datetime={phase.timestamp} title={phase.timestamp}
                  >{formatTimestamp(phase.timestamp, true)}</time
                >
              </div>
              {#if phase.detail}
                <p class="outcome-detail break-anywhere">{phase.detail}</p>
              {/if}
            </div>
          </div>
        </section>
      {/if}
    {/each}
  {/if}
</div>

<style>
  .job-execution-story {
    container-type: inline-size;
    min-width: 0;
  }

  .empty-story,
  .no-work-updates {
    color: color-mix(in oklab, var(--color-base-content) 68%, transparent);
    font-size: 0.8125rem;
    margin: 0;
  }

  .story-phase {
    border-top: 1px solid color-mix(in oklab, var(--color-base-300) 72%, transparent);
    padding-block: 1rem;
  }

  .story-phase:first-child {
    border-top: 0;
    padding-top: 0;
  }

  .story-phase:last-child {
    padding-bottom: 0;
  }

  .phase-heading {
    align-items: baseline;
    display: flex;
    gap: 0.75rem;
    justify-content: space-between;
    margin-bottom: 0.75rem;
  }

  .phase-heading h3,
  .work-heading,
  .execution-evidence-group > h4 {
    color: color-mix(in oklab, var(--color-base-content) 72%, transparent);
    font-size: 0.6875rem;
    font-weight: 750;
    letter-spacing: 0.09em;
    line-height: 1.25;
    margin: 0;
    text-transform: uppercase;
  }

  .phase-duration {
    color: var(--color-base-content);
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
    font-weight: 700;
    white-space: nowrap;
  }

  .event-type {
    color: color-mix(in oklab, var(--color-base-content) 68%, transparent);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.6875rem;
    font-weight: 650;
    letter-spacing: 0.02em;
  }

  .evidence-line,
  .step-evidence,
  .worker-line {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.6875rem;
    font-variant-numeric: tabular-nums;
    line-height: 1.45;
  }

  .evidence-line,
  .step-evidence {
    align-items: baseline;
    display: flex;
    flex-wrap: wrap;
    gap: 0.125rem 0.5rem;
  }

  .break-anywhere {
    overflow-wrap: anywhere;
  }

  .queue-events {
    display: grid;
    gap: 0.75rem;
    position: relative;
  }

  .queue-events::before {
    background: color-mix(in oklab, var(--color-info) 38%, var(--color-base-300));
    content: "";
    inset: 0.55rem auto 0.55rem 0.25rem;
    position: absolute;
    width: 1px;
  }

  .queue-event {
    align-items: start;
    display: grid;
    gap: 0.625rem;
    grid-template-columns: 0.5rem minmax(0, 1fr);
    position: relative;
  }

  .queue-event strong,
  .execution-summary > strong {
    color: var(--color-base-content);
    display: block;
    font-size: 0.875rem;
    font-weight: 680;
    line-height: 1.35;
    margin-bottom: 0.125rem;
  }

  .queue-marker {
    background: var(--color-info);
    border-radius: 999px;
    height: 0.5rem;
    margin-top: 0.3rem;
    position: relative;
    width: 0.5rem;
    z-index: 1;
  }

  .queue-marker-end {
    background: color-mix(in oklab, var(--color-info) 58%, var(--color-base-100));
    box-shadow: inset 0 0 0 1px var(--color-info);
  }

  .execution-summary {
    margin-bottom: 1rem;
  }

  .work-heading {
    margin: 0 0 0.75rem;
  }

  .worker-line {
    margin-top: 0.25rem;
  }

  .worker-line .trellis-identifier {
    overflow-wrap: anywhere;
    white-space: normal;
  }

  .work-steps,
  .dependency-waits,
  .runtime-events {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .log-entries {
    display: grid;
    gap: 0.5rem;
    list-style: none;
    margin: 0.5rem 0 0;
    padding: 0;
  }

  .log-entries li {
    align-items: baseline;
    color: var(--color-base-content);
    display: grid;
    font-size: 0.75rem;
    gap: 0.25rem 0.5rem;
    grid-template-columns: auto minmax(0, 1fr) auto;
    line-height: 1.4;
  }

  .log-entries time {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.6875rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .log-level {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.625rem;
    font-weight: 750;
    text-transform: uppercase;
  }

  .log-info {
    color: color-mix(in oklab, var(--color-info) 72%, var(--color-base-content));
  }

  .log-warn {
    color: color-mix(in oklab, var(--color-warning) 62%, var(--color-base-content));
  }

  .log-error {
    color: color-mix(in oklab, var(--color-error) 72%, var(--color-base-content));
  }

  .work-steps {
    display: grid;
  }

  .work-step {
    display: grid;
    gap: 0.75rem;
    grid-template-columns: 1.5rem minmax(0, 1fr);
    min-width: 0;
    padding-bottom: 1rem;
    position: relative;
  }

  .work-step:last-child {
    padding-bottom: 0;
  }

  .work-step:not(:last-child)::before {
    background: color-mix(in oklab, var(--color-base-300) 72%, transparent);
    content: "";
    inset: 1.5rem auto 0 0.735rem;
    position: absolute;
    width: 1px;
  }

  .step-rail span {
    align-items: center;
    background: color-mix(in oklab, var(--color-info) 8%, var(--color-base-100));
    border: 1px solid color-mix(in oklab, var(--color-info) 26%, var(--color-base-300));
    border-radius: 999px;
    color: color-mix(in oklab, var(--color-info) 78%, var(--color-base-content));
    display: flex;
    font-size: 0.625rem;
    font-variant-numeric: tabular-nums;
    font-weight: 750;
    height: 1.5rem;
    justify-content: center;
    position: relative;
    width: 1.5rem;
    z-index: 1;
  }

  .step-content {
    min-width: 0;
    padding-top: 0.0625rem;
  }

  .step-heading {
    align-items: baseline;
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem 0.75rem;
    justify-content: space-between;
  }

  .step-heading h4 {
    color: var(--color-base-content);
    flex: 1 1 12rem;
    font-size: 0.8125rem;
    font-weight: 620;
    line-height: 1.45;
    margin: 0;
    text-wrap: pretty;
  }

  .step-heading time {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.6875rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .step-evidence {
    margin-top: 0.25rem;
  }

  .dependency-waits {
    display: grid;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }

  .dependency-wait {
    background: color-mix(in oklab, var(--color-warning) 4%, transparent);
    border-radius: 0.5rem;
    min-width: 0;
    padding: 0.625rem 0.75rem;
    position: relative;
  }

  .dependency-wait::before {
    background: color-mix(in oklab, var(--color-warning) 46%, var(--color-base-300));
    content: "";
    height: 1px;
    left: -0.75rem;
    position: absolute;
    top: 1rem;
    width: 0.5rem;
  }

  .dependency-heading {
    align-items: baseline;
    display: flex;
    gap: 0.75rem;
    justify-content: space-between;
  }

  .dependency-kind {
    color: color-mix(in oklab, var(--color-warning) 62%, var(--color-base-content));
    font-size: 0.625rem;
    font-weight: 750;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }

  .dependency-duration {
    color: color-mix(in oklab, var(--color-warning) 58%, var(--color-base-content));
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .dependency-name {
    color: var(--color-base-content);
    font-size: 0.8125rem;
    font-weight: 650;
    line-height: 1.4;
    margin-top: 0.25rem;
  }

  .dependency-detail {
    color: color-mix(in oklab, var(--color-base-content) 68%, transparent);
    font-size: 0.6875rem;
    line-height: 1.45;
    margin-top: 0.125rem;
  }

  .dependency-timing {
    align-items: baseline;
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    display: grid;
    font-size: 0.6875rem;
    font-variant-numeric: tabular-nums;
    gap: 0.125rem 0.625rem;
    grid-template-columns: auto minmax(0, 1fr);
    margin-top: 0.375rem;
  }

  .dependency-timing time {
    white-space: nowrap;
  }

  .execution-waits {
    margin-top: 0.5rem;
  }

  .execution-waits .dependency-wait::before {
    display: none;
  }

  .execution-evidence-group {
    margin-top: 1rem;
  }

  .runtime-events {
    display: grid;
    gap: 0.75rem;
    margin-top: 0.625rem;
  }

  .runtime-event {
    border-top: 1px solid color-mix(in oklab, var(--color-base-300) 56%, transparent);
    padding-top: 0.625rem;
  }

  .runtime-event-heading {
    align-items: baseline;
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    justify-content: space-between;
  }

  .runtime-event-heading time,
  .runtime-event-detail {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
    font-size: 0.6875rem;
    font-variant-numeric: tabular-nums;
  }

  .runtime-event > strong {
    display: block;
    font-size: 0.8125rem;
    line-height: 1.4;
    margin-top: 0.125rem;
  }

  .runtime-event-detail {
    line-height: 1.45;
    margin: 0.25rem 0 0;
  }

  .outcome-content {
    align-items: start;
    display: grid;
    gap: 0.75rem;
    grid-template-columns: 0.75rem minmax(0, 1fr);
  }

  .outcome-marker {
    background: currentColor;
    border-radius: 999px;
    height: 0.625rem;
    margin-top: 0.3rem;
    width: 0.625rem;
  }

  .outcome-success {
    color: var(--color-success);
  }

  .outcome-error {
    color: var(--color-error);
  }

  .outcome-warning {
    color: var(--color-warning);
  }

  .outcome-neutral {
    color: color-mix(in oklab, var(--color-base-content) 64%, transparent);
  }

  .outcome-content h4 {
    color: var(--color-base-content);
    font-size: 0.9375rem;
    font-weight: 720;
    line-height: 1.35;
    margin: 0;
  }

  .outcome-content .evidence-line {
    margin-top: 0.25rem;
  }

  .outcome-detail {
    color: var(--color-base-content);
    font-size: 0.75rem;
    line-height: 1.45;
    margin: 0.5rem 0 0;
  }

  @container (max-width: 24rem) {
    .step-heading time {
      flex-basis: 100%;
    }

    .work-step {
      gap: 0.625rem;
    }

    .dependency-wait {
      padding-inline: 0.625rem;
    }

    .log-entries li {
      grid-template-columns: auto minmax(0, 1fr);
    }

    .log-entries time {
      grid-column: 2;
    }
  }
</style>
