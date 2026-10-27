// `fetch` against the same-origin API. The session is the `memo_session` cookie; the browser
// adds the `Origin` header the server checks on writes.

import { BackendError } from "./types";

const CLIENT = "web/0.1.0";

export async function call(method: string, path: string, body?: string | Uint8Array): Promise<Response> {
  const headers: Record<string, string> = { "Memo-Client": CLIENT };
  if (typeof body === "string") headers["Content-Type"] = "application/json";
  else if (body) headers["Content-Type"] = "application/octet-stream";
  let response: Response;
  try {
    response = await fetch(`/api/v1${path}`, {
      method,
      headers,
      body: body as BodyInit | undefined,
      credentials: "same-origin",
      cache: "no-store",
    });
  } catch {
    throw new BackendError("network", "The server cannot be reached.");
  }
  if (!response.ok) {
    let code = `http_${response.status}`;
    let message = response.statusText || `HTTP ${response.status}`;
    try {
      const error = (await response.json()).error;
      code = error.code;
      message = error.message;
    } catch {
      // Not an API error body (e.g. from the reverse proxy).
    }
    throw new BackendError(code, message, response.status);
  }
  return response;
}

/** JSON in and out, as text: memo-core parses and produces the bodies. */
export async function text(method: string, path: string, body?: string): Promise<string> {
  return (await call(method, path, body)).text();
}

export async function json<T>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await call(method, path, body === undefined ? undefined : JSON.stringify(body));
  return response.status === 204 ? (undefined as T) : response.json();
}

export async function bytes(path: string): Promise<Uint8Array> {
  return new Uint8Array(await (await call("GET", path)).arrayBuffer());
}
