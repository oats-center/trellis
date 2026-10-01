export function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

export async function postJson(
  url: string,
  body: Record<string, unknown>,
  headers: Record<string, string> = {},
): Promise<unknown> {
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      origin: new URL(url).origin,
      ...headers,
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    throw new Error(
      `Trellis HTTP request failed (${response.status}) for ${url}${
        text ? `: ${text}` : ""
      }`,
    );
  }
  const bytes = new Uint8Array(await response.arrayBuffer());
  try {
    return JSON.parse(new TextDecoder().decode(bytes));
  } catch {
    const responseUrl = new URL(response.url);
    // Account-flow paths and query parameters can contain credentials.
    responseUrl.pathname = responseUrl.pathname.replace(
      /(\/account-flow\/)[^/]+/,
      "$1<redacted>",
    );
    responseUrl.search = "";
    responseUrl.hash = "";
    responseUrl.username = "";
    responseUrl.password = "";
    throw new Error(
      `Trellis HTTP response was not valid JSON: ${
        JSON.stringify({
          status: response.status,
          url: responseUrl.toString(),
          redirected: response.redirected,
          contentType: response.headers.get("content-type"),
          contentLength: response.headers.get("content-length"),
          bodyByteLength: bytes.byteLength,
        })
      }`,
    );
  }
}
