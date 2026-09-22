export function estimateMidpointClockOffsetMs(args: {
  requestStartedAtMs: number;
  responseReceivedAtMs: number;
  serverNowSeconds: number;
}): number {
  const midpointMs = (args.requestStartedAtMs + args.responseReceivedAtMs) / 2;
  return args.serverNowSeconds * 1_000 - midpointMs;
}

export function correctedIatSeconds(
  nowMs: number = Date.now(),
  clockOffsetMs: number = 0,
): number {
  return Math.floor((nowMs + clockOffsetMs) / 1_000);
}
