// The browser's `Backend`: a proxy that forwards each call to the worker.

import { download } from "./format";
import type { Backend, BackendEvent } from "./types";
import { BackendError } from "./types";

export function webBackend(): Backend {
  const worker = new Worker(new URL("./worker.ts", import.meta.url), { type: "module", name: "memo" });
  const calls = new Map<number, { resolve: (value: unknown) => void; reject: (error: unknown) => void }>();
  const listeners: ((event: BackendEvent) => void)[] = [];
  let next = 0;

  worker.addEventListener("message", (event: MessageEvent) => {
    const message = event.data;
    if (message.event) {
      for (const listener of listeners) listener(message.event);
      return;
    }
    const call = calls.get(message.id);
    calls.delete(message.id);
    if (!call) return;
    if (message.error) call.reject(new BackendError(message.error.code, message.error.message, message.error.status));
    else call.resolve(message.result);
  });

  const invoke =
    (method: string) =>
    (...args: unknown[]) =>
      new Promise((resolve, reject) => {
        const id = next++;
        calls.set(id, { resolve, reject });
        worker.postMessage({ id, method, args });
      });

  const local: Partial<Backend> = {
    native: false,
    on: (listener) => void listeners.push(listener),
    saveFile: async (bytes, name, type) => {
      download(bytes, name, type);
      return null;
    },
    openLink: (url) => void window.open(url, "_blank", "noopener,noreferrer"),
  };

  return new Proxy({} as Backend, {
    get(_, name) {
      if (name in local) return local[name as keyof Backend];
      // Not a thenable.
      if (typeof name !== "string" || name === "then") return undefined;
      return invoke(name);
    },
  });
}
