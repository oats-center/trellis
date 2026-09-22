export type SessionRow = {
  origin: string;
  id: string;
  sessionKey: string;
};

export type ConnectionRow = SessionRow & {
  userNkey: string;
};

export function parseSessionRowKey(value: string): SessionRow | null {
  const parts = value.split(".");
  if (parts.length < 3) return null;
  const origin = parts[0] ?? "";
  const sessionKey = parts.at(-1) ?? "";
  const id = parts.slice(1, -1).join(".");
  if (!origin || !id || !sessionKey) return null;
  return { origin, id, sessionKey };
}

export function parseConnectionRowKey(value: string): ConnectionRow | null {
  const parts = value.split(".");
  if (parts.length < 4) return null;
  const origin = parts[0] ?? "";
  const userNkey = parts.at(-1) ?? "";
  const sessionKey = parts.at(-2) ?? "";
  const id = parts.slice(1, -2).join(".");
  if (!origin || !id || !sessionKey || !userNkey) return null;
  return { origin, id, sessionKey, userNkey };
}

export function formatOriginId(origin: string, id: string): string {
  return `${origin}.${id}`;
}
