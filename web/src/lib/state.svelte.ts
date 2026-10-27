// App-wide state: the session, sync status, notifications, the list filter and the route.

import { webBackend } from "./backend";
import type { Backend, Session, SyncEvent, SyncStatus } from "./types";
import { BackendError } from "./types";

export const backend: Backend = webBackend();

export interface Toast {
  id: number;
  kind: "info" | "warning" | "danger";
  text: string;
  memoId?: string;
}

export const app = $state({
  session: null as Session | null,
  sync: { syncing: false, last: null, error: null, unsynced: false } as SyncStatus,
  /** Bumped whenever data may have changed; views reload on it. */
  version: 0,
  toasts: [] as Toast[],
});

export const filter = $state({ query: "", tag: null as string | null, archived: false });

/** `#/memo/<id>` or `#/settings`; anything else is the list. */
export const route = $state({ memo: null as string | null, settings: false });

function parseRoute() {
  const hash = decodeURIComponent(location.hash.slice(1));
  route.memo = /^\/memo\/([0-9a-f-]{36})$/.exec(hash)?.[1] ?? null;
  route.settings = hash === "/settings";
}
parseRoute();
window.addEventListener("hashchange", parseRoute);

export function go(path: string) {
  location.hash = path;
}

let toastId = 0;

export function toast(kind: Toast["kind"], text: string, memoId?: string) {
  const id = toastId++;
  app.toasts.push({ id, kind, text, memoId });
  if (kind === "info") setTimeout(() => dismiss(id), 6000);
}

export function dismiss(id: number) {
  const index = app.toasts.findIndex((t) => t.id === id);
  if (index >= 0) app.toasts.splice(index, 1);
}

/** What to tell the user about a sync event, if anything. */
function describe(event: SyncEvent): [Toast["kind"], string, string?] | null {
  switch (event.type) {
    case "reset":
      return ["info", "The server asked for a full resync. Everything is up to date again."];
    case "conflict":
      return ["warning", "A memo was edited elsewhere at the same time. Choose what to keep.", event.memo_id];
    case "restored_copy":
      return [
        "warning",
        event.reason === "deleted"
          ? "A memo you edited was deleted elsewhere. Your edit was saved as a new memo."
          : "You lost access to a memo you edited. Your edit was saved as a new private memo.",
        event.copy,
      ];
    case "delete_rejected":
      return ["info", "A memo was not deleted because it changed elsewhere.", event.memo_id];
    case "push_rejected":
      return [
        "danger",
        event.reason === "quota_exceeded"
          ? "Your storage quota is full. Some changes stay on this device until you free up space."
          : event.reason === "forbidden"
            ? "You can no longer edit a memo. Your changes are kept here; save them as a copy."
            : "The server refused a change.",
        event.memo_id,
      ];
    case "server_lost_data":
      return ["info", "The server had an older copy of a memo. This device's version was sent back.", event.memo_id];
    case "cannot_decrypt":
      return ["danger", "A memo cannot be decrypted.", event.memo_id];
    case "newer_format":
      return ["warning", "A memo was saved by a newer app version. Update to edit it.", event.memo_id];
    case "key_rejected":
      return ["danger", "A shared memo's key did not verify against its owner's key. It was not opened.", event.memo_id];
    case "key_changed":
      return ["danger", "The server presented a different key for one of your contacts. Check it before sharing."];
    case "pin_conflict":
      return ["danger", "Two of your devices saw different keys for the same contact. Verify it."];
    case "vault_newer_format":
      return ["warning", "Your contact list was saved by a newer app version. Update the app."];
    case "vault_undecryptable":
      return ["danger", "Your contact list cannot be decrypted."];
    case "rollback":
      return null;
  }
}

backend.on((event) => {
  switch (event.type) {
    case "session":
      app.session = event.session;
      break;
    case "sync":
      app.sync = event.status;
      break;
    case "changed":
      app.version++;
      break;
    case "events":
      for (const e of event.events) {
        const described = describe(e);
        if (described) toast(...described);
      }
      break;
  }
});

document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") backend.poke();
});
window.addEventListener("focus", () => backend.poke());
window.addEventListener("beforeunload", (event) => {
  if (app.sync.unsynced) event.preventDefault();
});

const MESSAGES: Record<string, string> = {
  network: "The server cannot be reached.",
  invalid_credentials: "Wrong username or password.",
  invite_invalid: "This invite code is unknown, expired or already used.",
  rate_limited: "Too many attempts. Wait a little and try again.",
  recovery_key_invalid: "This recovery key is not valid. Check it for typos.",
  key_changed: "This user's key differs from the one you saw before. Do not share until you have checked it with them.",
  not_pinned: "A member's key is not known on this device yet. Sync and try again.",
  read_only: "You cannot edit this memo.",
  in_conflict: "Resolve the conflict first.",
  not_owner: "Only the owner can do this.",
  quota_exceeded: "Your storage quota is full.",
  too_large: "This is too large.",
  malformed: "This file is not a supported image (JPEG, PNG, WebP or GIF).",
  bad_image: "This file is not a supported image (JPEG, PNG, WebP or GIF).",
  decrypt: "Decryption failed.",
  kdf_params: "The server sent unacceptable key derivation parameters.",
};

export function errorText(error: unknown, overrides: Record<string, string> = {}): string {
  if (error instanceof BackendError) return overrides[error.code] ?? MESSAGES[error.code] ?? error.message;
  return error instanceof Error ? error.message : String(error);
}

export function errorCode(error: unknown): string | null {
  return error instanceof BackendError ? error.code : null;
}

export async function loadSession() {
  try {
    app.session = await backend.session();
  } catch {
    app.session = { state: "offline" };
  }
}
