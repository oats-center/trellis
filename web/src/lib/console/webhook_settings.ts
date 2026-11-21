/** What a webhook runtime setting measures, which decides its units. */
export type WebhookSettingUnit = "count" | "ms" | "bytes";

const DURATIONS = [
  [86_400_000, "d"],
  [3_600_000, "h"],
  [60_000, "m"],
  [1000, "s"],
] as const;

// Maps, so a suffix such as `constructor` finds nothing rather than an
// inherited property.
const MULTIPLIERS: Record<WebhookSettingUnit, Map<string, bigint>> = {
  count: new Map(),
  ms: new Map([
    ["ms", 1n],
    ["s", 1000n],
    ["m", 60_000n],
    ["h", 3_600_000n],
    ["d", 86_400_000n],
  ]),
  bytes: new Map([
    ["B", 1n],
    ["KiB", 1024n],
    ["MiB", 1_048_576n],
    ["GiB", 1_073_741_824n],
  ]),
};

/**
 * Shows a setting's stored value in the largest whole unit, such as `5m` or
 * `512 KiB`.
 */
export function showWebhookSetting(
  value: unknown,
  unit: WebhookSettingUnit,
): string {
  const number = Number(value);
  if (unit === "bytes") {
    if (number % (1 << 20) === 0) return `${number / (1 << 20)} MiB`;
    if (number % 1024 === 0) return `${number / 1024} KiB`;
    return `${number} B`;
  }
  if (unit === "ms") {
    for (const [size, word] of DURATIONS) {
      if (number >= size && number % size === 0)
        return `${number / size}${word}`;
    }
    return `${number} ms`;
  }
  return String(number);
}

/**
 * Parses what someone typed for a setting into its stored integer. Only the
 * setting's own units are accepted, so `5m` is refused for a size and
 * `512KiB` for a duration; a bare number is milliseconds, bytes, or the
 * count. Returns `null` when the text is not a value of that setting.
 */
export function parseWebhookSetting(
  text: string,
  unit: WebhookSettingUnit,
): bigint | null {
  const match = /^(\d+)\s*([A-Za-z]+)?$/u.exec(text.trim());
  if (!match) return null;
  const suffix = match[2];
  if (suffix === undefined) return BigInt(match[1]!);
  const multiplier = MULTIPLIERS[unit].get(suffix);
  return multiplier === undefined ? null : BigInt(match[1]!) * multiplier;
}
