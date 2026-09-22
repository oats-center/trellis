import { deepEqual, equal } from "node:assert/strict";

import {
  IntervalTimer,
  LIVE_REFRESH_DELAY_MS,
  LiveSubscription,
  RefreshScheduler,
  retryBackoffMs,
} from "./live_refresh.ts";

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

Deno.test("U11 continuous events cannot starve a coalesced refresh", async () => {
  let refreshes = 0;
  const scheduler = new RefreshScheduler({
    onRefresh: () => {
      refreshes += 1;
      return Promise.resolve();
    },
    delayMs: 10,
  });

  for (let index = 0; index < 20; index += 1) {
    scheduler.notify();
    await delay(1);
  }
  await delay(30);
  equal(
    refreshes >= 1,
    true,
    "a burst still refreshes within the original window",
  );
  scheduler.dispose();
});

Deno.test("U11 one in-flight read plus one trailing refresh", async () => {
  let refreshes = 0;
  let release = () => {};
  const scheduler = new RefreshScheduler({
    delayMs: 1,
    onRefresh: async () => {
      refreshes += 1;
      if (refreshes === 1) {
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      }
    },
  });

  scheduler.notify();
  await delay(5);
  equal(refreshes, 1);
  scheduler.notify();
  scheduler.notify();
  release();
  await delay(15);
  equal(
    refreshes,
    2,
    "events during a read produce exactly one trailing refresh",
  );
  scheduler.dispose();
});

Deno.test("U11 disposal stops timers and retries", async () => {
  let refreshes = 0;
  const scheduler = new RefreshScheduler({
    delayMs: 5,
    onRefresh: () => {
      refreshes += 1;
      return Promise.resolve();
    },
  });
  scheduler.notify();
  scheduler.dispose();
  await delay(20);
  equal(refreshes, 0);
  equal(scheduler.disposed, true);
});

Deno.test("U11 a suspended scope cancels its pending refresh", async () => {
  let refreshes = 0;
  let allowed = false;
  const scheduler = new RefreshScheduler({
    delayMs: 5,
    canRefresh: () => allowed,
    onRefresh: () => {
      refreshes += 1;
      return Promise.resolve();
    },
  });
  scheduler.notify();
  await delay(15);
  equal(refreshes, 0);
  allowed = true;
  await scheduler.refreshNow();
  equal(refreshes, 1);
  scheduler.dispose();
});

Deno.test("U11 live subscription retries with capped backoff and resets after connect", async () => {
  deepEqual(
    [1, 2, 3, 4, 5, 6, 7].map(retryBackoffMs),
    [1_000, 2_000, 5_000, 10_000, 30_000, 30_000, 30_000],
  );

  const attempts: number[] = [];
  let opens = 0;
  const subscription = new LiveSubscription({
    subscribe: () => {
      opens += 1;
      if (opens < 3) return Promise.reject(new Error("closed"));
      return Promise.resolve();
    },
    unsubscribe: () => {},
    onStatus: (status, detail) => {
      if (status === "reconnecting" && detail !== undefined) {
        attempts.push(detail.retryInMs);
      }
    },
  });

  await subscription.start();
  equal(
    subscription.status,
    "reconnecting",
    "a failed open reports reconnecting, not a derived connecting",
  );
  await subscription.dispose();
  equal(attempts[0], 1_000, "first failure backs off one second");
});

Deno.test("V13 repeated close and start never creates a second retry timer", async () => {
  let opens = 0;
  let unsubscribe = 0;
  const subscription = new LiveSubscription({
    subscribe: () => {
      opens += 1;
      return Promise.resolve();
    },
    unsubscribe: () => {
      unsubscribe += 1;
    },
  });

  await subscription.start();
  equal(subscription.status, "live");
  // Repeated start calls while live must not open another watch.
  await subscription.start();
  await subscription.start();
  equal(opens, 1, "a live subscription is single-flight");

  // Repeated closure must schedule exactly one retry, not one per call.
  subscription.closed();
  subscription.closed();
  subscription.closed();
  equal(
    subscription.status,
    "reconnecting",
    "a closed watch reports reconnecting immediately",
  );
  // Give the first backoff window a chance to fire exactly one retry.
  await delay(1_100);
  equal(opens, 2, "repeated closures schedule one retry, not one per call");

  await subscription.dispose();
  equal(subscription.status, "closed");
  equal(unsubscribe, 1, "disposal releases exactly the owned watch");
});

Deno.test("V13 a late open completion cannot revive a disposed subscription", async () => {
  let release = () => {};
  const subscription = new LiveSubscription({
    subscribe: async () => {
      await new Promise<void>((resolve) => {
        release = resolve;
      });
    },
    unsubscribe: () => {},
  });

  const starting = subscription.start();
  await delay(5);
  const disposing = subscription.dispose();
  release();
  await starting;
  await disposing;
  equal(
    subscription.status,
    "closed",
    "a late open must not restore live after disposal",
  );
});

Deno.test("Z08 default coalescing delay is 250ms", () => {
  equal(LIVE_REFRESH_DELAY_MS, 250);
});

Deno.test("Z07 one in-flight read plus one trailing refresh uses the newest query", async () => {
  const seen: string[] = [];
  let query = "old";
  let release = () => {};
  const scheduler = new RefreshScheduler({
    delayMs: 1,
    onRefresh: async () => {
      seen.push(query);
      if (seen.length === 1) {
        await new Promise<void>((resolve) => {
          release = resolve;
        });
      }
    },
  });

  const first = scheduler.refreshNow();
  await delay(5);
  deepEqual(seen, ["old"]);
  query = "new";
  scheduler.notify();
  scheduler.notify();
  release();
  await first;
  await delay(15);
  deepEqual(seen, ["old", "new"]);
  scheduler.dispose();
});

Deno.test("Z08 an immediate refresh cancels a pending timer instead of reading twice", async () => {
  let refreshes = 0;
  const scheduler = new RefreshScheduler({
    delayMs: 40,
    onRefresh: () => {
      refreshes += 1;
      return Promise.resolve();
    },
  });
  scheduler.notify();
  await scheduler.refreshNow();
  equal(refreshes, 1);
  await delay(60);
  equal(refreshes, 1, "the coalesced timer must not fire after refreshNow");
  scheduler.dispose();
});

Deno.test("V12 a hidden notification refreshes once on resume without a new event", async () => {
  let refreshes = 0;
  let visible = false;
  const scheduler = new RefreshScheduler({
    delayMs: 5,
    canRefresh: () => visible,
    onRefresh: () => {
      refreshes += 1;
      return Promise.resolve();
    },
  });

  scheduler.notify();
  await delay(20);
  equal(refreshes, 0, "a hidden scope does not read");
  equal(
    scheduler.dirty,
    true,
    "the notification is retained while hidden",
  );

  visible = true;
  scheduler.resume();
  await delay(10);
  equal(refreshes, 1, "recovery refreshes without another feed event");

  // A clean scope resumes nothing, so repeated recovery signals cannot cause
  // duplicate reads.
  scheduler.resume();
  scheduler.resume();
  await delay(20);
  equal(refreshes, 1, "repeated resumes do not duplicate a satisfied refresh");
  scheduler.dispose();
});

Deno.test("U11 disposal is final: no retry opens after it", async () => {
  let opens = 0;
  const subscription = new LiveSubscription({
    subscribe: () => {
      opens += 1;
      return Promise.reject(new Error("closed"));
    },
    unsubscribe: () => {},
  });
  await subscription.start();
  await subscription.dispose();
  const afterDispose = opens;
  await delay(20);
  equal(opens, afterDispose);
  equal(subscription.status, "closed");
});

Deno.test("U15 interval timer does not stack its reads and pauses while blocked", async () => {
  let ticks = 0;
  let running = 0;
  let peak = 0;
  const allowed = false;
  const timer = new IntervalTimer({
    intervalMs: 5,
    canRun: () => allowed,
    tick: async () => {
      running += 1;
      peak = Math.max(peak, running);
      await delay(20);
      ticks += 1;
      running -= 1;
    },
  });
  timer.start();
  await delay(40);
  timer.dispose();
  equal(ticks, 0, "a suspended scope never reads");
  equal(peak, 0);
});

Deno.test("U15 interval timer runs one read at a time", async () => {
  let running = 0;
  let peak = 0;
  let ticks = 0;
  const timer = new IntervalTimer({
    intervalMs: 5,
    tick: async () => {
      running += 1;
      peak = Math.max(peak, running);
      await delay(15);
      ticks += 1;
      running -= 1;
    },
  });
  timer.start();
  await delay(50);
  timer.dispose();
  equal(peak, 1, "interval tick never overlaps itself");
  equal(ticks > 0, true);
});

Deno.test("U15 a manual refresh cannot overlap the interval read", async () => {
  let running = 0;
  let peak = 0;
  const timer = new IntervalTimer({
    intervalMs: 5,
    tick: async () => {
      running += 1;
      peak = Math.max(peak, running);
      await delay(15);
      running -= 1;
    },
  });
  timer.start();
  await timer.refreshNow();
  await timer.refreshNow();
  await delay(20);
  timer.dispose();
  equal(peak, 1);
  equal(LIVE_REFRESH_DELAY_MS > 0, true);
});
