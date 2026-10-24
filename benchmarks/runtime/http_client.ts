import {
  Agent as HttpAgent,
  request as httpRequest,
  type IncomingMessage,
} from "node:http";
import { Agent as HttpsAgent, request as httpsRequest } from "node:https";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { signRequest, type HttpTlsMaterial } from "./http_security.ts";

/**
 * Node HTTP/1.1 client used for every HTTP workload. One keep-alive agent per
 * client; `fresh` opens a new connection (and TLS handshake) per request.
 * Requests carry a declared content length, never chunked encoding, so both
 * languages send the same framing and sign the same declared size.
 */
export function createHttpClient(
  baseUrl: string,
  tls: HttpTlsMaterial | undefined,
  fresh = false,
) {
  const url = new URL(baseUrl);
  const secure = url.protocol === "https:";
  if (secure !== Boolean(tls)) {
    throw new Error("HTTP scheme and TLS material disagree");
  }
  const identity = tls
    ? { ca: tls.ca, cert: tls.clientCert, key: tls.clientKey }
    : {};
  const agent = fresh
    ? undefined
    : secure
      ? new HttpsAgent({ keepAlive: true, ...identity })
      : new HttpAgent({ keepAlive: true });
  return {
    /** Send one request; the caller consumes the returned response body. */
    send(
      method: "GET" | "PUT",
      target: string,
      body?: { chunks: AsyncIterable<Uint8Array>; length: number },
    ): Promise<IncomingMessage> {
      return new Promise((resolve, reject) => {
        const request = (secure ? httpsRequest : httpRequest)(
          {
            method,
            host: url.hostname,
            port: url.port,
            path: target,
            agent: agent ?? false,
            ...identity,
            signal: AbortSignal.timeout(30_000),
            headers: {
              "accept-encoding": "identity",
              ...(body ? { "content-length": String(body.length) } : {}),
              ...(tls ? signRequest(tls, method, target, body?.length) : {}),
            },
          },
          resolve,
        );
        request.once("error", reject);
        if (body) pipeline(Readable.from(body.chunks), request).catch(reject);
        else request.end();
      });
    },
    close() {
      agent?.destroy();
    },
  };
}
