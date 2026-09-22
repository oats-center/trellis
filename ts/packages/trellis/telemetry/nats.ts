import type { HeaderCarrier } from "./carrier.ts";

export interface NatsHeadersLike {
  get(key: string): string | undefined;
  set(key: string, value: string): void;
}

export function createNatsHeaderCarrier(
  headers: NatsHeadersLike,
): HeaderCarrier {
  return {
    get: (key: string) => headers.get(key),
    set: (key: string, value: string) => headers.set(key, value),
  };
}
