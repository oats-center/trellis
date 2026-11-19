---
title: "Upgrade a service without downtime"
description: "Start the new build beside the old one, wait until it serves, then stop the old one, and what happens to work in flight."
---

# Upgrade a service without downtime

Trellis services upgrade by overlapping. Start the new build beside the old
one, wait until it serves, then stop the old one. Every instance of a deployment
joins the same queue groups, so while both run, requests, jobs, and event
deliveries go to either of them.

## What a stop did in 0.100.0-rc.50

Measured on 0.100.0-rc.50 with a caller sending one request every 100 ms, the
same in TypeScript and Rust. The old instance was stopped while it held a
2-second request, a running job, an open Live stream, and an event delivery,
and a second instance was serving.

- **SIGTERM ends the process at once.** Neither SDK handled the signal; the
  runtime's default ended the process. `service.stop()` in TypeScript returned
  within about 50 ms.
- **The slow request is lost.** When its own timeout ran out, its caller got
  `TransportError: The request deadline elapsed.` A request the broker had
  already handed to the old instance in the last moment can be lost the same
  way: one in 300 in one run, none in the others.
- **The running job resumes on the other instance after a few seconds**:
  about 4 s after SIGTERM, about 5 s after `service.stop()`, which cancels the
  handler and puts the job back with its retry delay.
- **The event delivery waits for its acknowledgement deadline**, 30 s by
  default, before the consumer delivers it again, to the other instance.
- **The Live stream does not move.** About 33 s later its caller saw
  `LiveStreamError: live provider silent past the inactivity bound` and had to
  open it again.
- New requests went to the other instance with no failures.
