import { base64urlEncode, sha256, utf8 } from "./utils.ts";

export async function trellisIdFromOriginId(
  origin: string,
  id: string,
): Promise<string> {
  const digest = await sha256(utf8(`${origin}:${id}`));
  return base64urlEncode(digest).slice(0, 22);
}
