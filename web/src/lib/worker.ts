// The web backend: memo-core (WASM) in a worker, so Argon2 and decryption never block the page.
// It owns the unlocked account and the in-memory store, talks to the API, and schedules sync
// (`docs/sync.md#when-sync-runs`). The page calls the methods below over `postMessage`.

import init, * as wasm from "../wasm/memo.js";
import wasmUrl from "../wasm/memo_bg.wasm?url";
import * as http from "./http";
import { exportPaths, extension, linkExported } from "./exported";
import { prepareImage } from "./images";
import type {
  AttachedImage,
  AttachmentKind,
  BackendEvent,
  Device,
  Me,
  MemoDetail,
  MemoFilter,
  MemoSummary,
  Permission,
  RevisionInfo,
  SavedFilter,
  Session,
  Stats,
  SyncEvent,
  SyncStatus,
  TagCount,
  UserInfo,
} from "./types";
import { BackendError } from "./types";
import { zip } from "./zip";

interface WorkerScope {
  postMessage(message: unknown, transfer?: Transferable[]): void;
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  addEventListener(type: "online", listener: () => void): void;
}
const scope = globalThis as unknown as WorkerScope;

const EDIT_DELAY = 2_000;
const PERIODIC = 5 * 60_000;
const MAX_BACKOFF = 5 * 60_000;

const ready = init({ module_or_path: wasmUrl });

let client: wasm.Client | null = null;
let me: Me | null = null;
/** After register or recover: unlocked, but the user has not confirmed the recovery key yet. */
let onboarding: wasm.Client | null = null;

function emit(event: BackendEvent) {
  scope.postMessage({ event });
}

function now() {
  return Date.now();
}

function device(): string {
  const ua = navigator.userAgent;
  const browser = /Firefox\//.test(ua)
    ? "Firefox"
    : /Edg\//.test(ua)
      ? "Edge"
      : /Chrome\//.test(ua)
        ? "Chrome"
        : /Safari\//.test(ua)
          ? "Safari"
          : "Browser";
  const os = /Android/.test(ua)
    ? "Android"
    : /iPhone|iPad/.test(ua)
      ? "iOS"
      : /Mac OS X/.test(ua)
        ? "macOS"
        : /Windows/.test(ua)
          ? "Windows"
          : /Linux/.test(ua)
            ? "Linux"
            : "";
  return JSON.stringify({ name: os ? `${browser} on ${os}` : browser, kind: "web" });
}

function unlocked(): wasm.Client {
  if (!client) throw new BackendError("locked", "Unlock first.");
  return client;
}

// ---- Session ----

async function session(): Promise<Session> {
  await ready;
  if (client && me) return { state: "unlocked", me, fingerprint: client.fingerprint() };
  try {
    const current = await http.json<Me>("GET", "/me");
    return { state: "locked", username: current.username };
  } catch (e) {
    if (e instanceof BackendError && e.code === "unauthenticated") return { state: "signed_out" };
    if (e instanceof BackendError && e.code === "network") return { state: "offline" };
    throw e;
  }
}

async function start(next: wasm.Client) {
  client?.free();
  client = next;
  me = await http.json<Me>("GET", "/me");
  emit({ type: "session", session: await session() });
  void runSync();
}

function signedOut() {
  client?.free();
  onboarding?.free();
  client = onboarding = null;
  me = null;
  clearTimeout(syncTimer);
  emit({ type: "session", session: { state: "signed_out" } });
}

async function login(username: string, password: string) {
  await ready;
  const prelogin = await http.text("POST", "/auth/prelogin", JSON.stringify({ username }));
  const keys = new wasm.PasswordLogin(password, prelogin);
  try {
    // With a valid session cookie (unlocking after a reload) this replaces the device.
    const response = await http.text("POST", "/auth/login", keys.request(username, device()));
    await start(keys.unlock(response));
  } finally {
    keys.free();
  }
}

async function unlock(password: string) {
  const current = await http.json<Me>("GET", "/me");
  await login(current.username, password);
}

async function register(username: string, password: string, invite: string): Promise<string> {
  await ready;
  const registration = new wasm.Registration(password);
  try {
    await http.text("POST", "/auth/register", registration.request(username, invite, device()));
    const recoveryKey = registration.recovery_key();
    onboarding = registration.into_client();
    return recoveryKey;
  } catch (e) {
    registration.free();
    throw e;
  }
}

async function recover(username: string, recoveryKey: string, newPassword: string): Promise<string> {
  await ready;
  const recovery = new wasm.Recovery(recoveryKey);
  try {
    const response = await http.text("POST", "/auth/recover", recovery.request(username));
    const completion = recovery.complete(response, newPassword, device());
    await http.text("POST", "/auth/recover/complete", completion.request());
    const newKey = completion.recovery_key();
    onboarding = completion.into_client();
    return newKey;
  } finally {
    recovery.free();
  }
}

async function finishOnboarding() {
  if (!onboarding) throw new BackendError("not_found", "Nothing to finish.");
  const next = onboarding;
  onboarding = null;
  await start(next);
}

async function logout() {
  try {
    await http.call("POST", "/auth/logout");
  } finally {
    signedOut();
  }
}

async function lock() {
  client?.free();
  client = null;
  me = null;
  clearTimeout(syncTimer);
  emit({ type: "session", session: await session() });
}

// ---- Sync ----

let status: SyncStatus = { syncing: false, last: null, error: null, unsynced: false };
let syncTimer: ReturnType<typeof setTimeout> | undefined;
let running: Promise<void> | null = null;
let again = false;
let failures = 0;

function setStatus(update: Partial<SyncStatus>) {
  status = { ...status, ...update };
  emit({ type: "sync", status });
}

function scheduleSync(delay: number) {
  if (!client) return;
  clearTimeout(syncTimer);
  syncTimer = setTimeout(() => void runSync(), delay);
}

/** Runs a sync now, or once more after the one in progress. */
function runSync(): Promise<void> {
  if (running) {
    again = true;
    return running;
  }
  running = (async () => {
    do {
      again = false;
      await syncRound();
    } while (again && client);
  })().finally(() => (running = null));
  return running;
}

async function syncRound() {
  const current = client;
  if (!current) return;
  clearTimeout(syncTimer);
  setStatus({ syncing: true });
  try {
    const events = await syncOnce(current);
    failures = 0;
    setStatus({ last: now(), error: null });
    if (events.length) emit({ type: "events", events });
    emit({ type: "changed" });
    scheduleSync(PERIODIC);
  } catch (e) {
    if (e instanceof BackendError && e.code === "unauthenticated") {
      signedOut();
      return;
    }
    failures++;
    setStatus({ error: e instanceof Error ? e.message : String(e) });
    scheduleSync(Math.min(MAX_BACKOFF, 5_000 * 2 ** (failures - 1)));
  } finally {
    setStatus({ syncing: false, unsynced: client?.has_unsynced() ?? false });
  }
}

async function syncOnce(current: wasm.Client): Promise<SyncEvent[]> {
  let body: string | undefined = current.sync_start(now());
  let events: SyncEvent[];
  try {
    while (body !== undefined) {
      const response = await http.text("POST", "/sync", body);
      body = current.sync_handle(response);
    }
  } finally {
    events = JSON.parse(current.sync_finish());
  }
  const uploads: { memo_id: string; attachment_id: string; kind: AttachmentKind }[] = JSON.parse(
    current.pending_uploads(),
  );
  for (const upload of uploads) {
    const blob = current.blob(upload.attachment_id, upload.kind);
    if (!blob) continue;
    try {
      await http.call("PUT", `/memos/${upload.memo_id}/attachments/${upload.attachment_id}/${upload.kind}`, blob);
      current.upload_done(upload.attachment_id, upload.kind);
    } catch (e) {
      // Too large or over quota: retrying every sync will not help; the image stays local.
      if (e instanceof BackendError && (e.code === "too_large" || e.code === "quota_exceeded")) {
        events.push({ type: "push_rejected", memo_id: upload.memo_id, reason: "quota_exceeded" });
        continue;
      }
      throw e;
    }
  }
  return events;
}

/** A local change: sync soon, and tell the page. */
function edited() {
  emit({ type: "changed" });
  setStatus({ unsynced: client?.has_unsynced() ?? false });
  // A round already running pushed what was dirty when it started: run one more after it.
  if (running) again = true;
  else scheduleSync(EDIT_DELAY);
}

scope.addEventListener("online", () => scheduleSync(0));

// ---- Memos ----

const downloads = new Map<string, Promise<void>>();

async function attachment(memoId: string, attachmentId: string, kind: AttachmentKind) {
  const current = unlocked();
  let bytes = current.attachment(memoId, attachmentId, kind);
  if (!bytes) {
    const key = `${attachmentId}/${kind}`;
    let download = downloads.get(key);
    if (!download) {
      download = http
        .bytes(`/attachments/${attachmentId}/${kind}`)
        .then((envelope) => current.put_blob(attachmentId, kind, envelope))
        .finally(() => downloads.delete(key));
      downloads.set(key, download);
    }
    try {
      await download;
    } catch (e) {
      // Not uploaded yet.
      if (e instanceof BackendError && e.code === "not_found") return null;
      throw e;
    }
    bytes = current.attachment(memoId, attachmentId, kind);
    if (!bytes) throw new BackendError("decrypt", "The image cannot be decrypted.");
  }
  const detail: MemoDetail = JSON.parse(current.memo_detail(memoId));
  const view = detail.attachments.find((a) => a.id === attachmentId);
  const mime = kind === "thumbnail" ? view?.thumb?.mime : view?.mime;
  return { bytes, renderable: mime !== undefined && wasm.image_renderable(bytes, mime) };
}

async function clean(bytes: Uint8Array) {
  const image = wasm.clean_image(bytes);
  try {
    return {
      mime: image.mime(),
      width: image.width(),
      height: image.height(),
      orientation: image.orientation(),
      bytes: image.bytes(),
    };
  } finally {
    image.free();
  }
}

async function attachImage(memoId: string, file: Blob, maxSide: number | null): Promise<AttachedImage> {
  const image = await prepareImage(file, maxSide, clean);
  const id = unlocked().add_attachment(
    memoId,
    image.mime,
    image.width,
    image.height,
    image.bytes,
    image.thumb?.mime,
    image.thumb?.width ?? 0,
    image.thumb?.height ?? 0,
    image.thumb?.bytes,
  );
  edited();
  return { id, width: image.width, height: image.height };
}

// ---- Sharing ----

async function lookupUser(username: string) {
  const user = await http.json<UserInfo>("GET", `/users/lookup?username=${encodeURIComponent(username.trim())}`);
  const trust = JSON.parse(unlocked().contact_trust(JSON.stringify(user)));
  return { ...user, ...trust };
}

async function share(memoId: string, user: UserInfo, permission: Permission) {
  // The memo must be on the server.
  await runSync();
  const request = unlocked().prepare_share(memoId, JSON.stringify(user), permission, now());
  await http.text("POST", `/memos/${memoId}/members`, request);
  await runSync();
}

async function revoke(memoId: string, userIds: string[]) {
  for (let attempt = 0; ; attempt++) {
    await runSync();
    const request = unlocked().prepare_revoke(memoId, JSON.stringify(userIds));
    try {
      const response = await http.text("POST", `/memos/${memoId}/revoke`, request);
      unlocked().apply_revoke(memoId, response);
      break;
    } catch (e) {
      // Someone wrote in the meantime: sync and re-encrypt.
      if (!(e instanceof BackendError && e.code === "conflict" && attempt < 2)) throw e;
    }
  }
  await runSync();
}

// ---- Account ----

async function prelogin(): Promise<string> {
  if (!me) throw new BackendError("locked", "Unlock first.");
  return http.text("POST", "/auth/prelogin", JSON.stringify({ username: me.username }));
}

// ---- Export ----

async function exportAll(): Promise<Uint8Array> {
  const current = unlocked();
  const files: { name: string; bytes: Uint8Array }[] = [];
  const encoder = new TextEncoder();
  const all: MemoSummary[] = [
    ...JSON.parse(current.memo_list(JSON.stringify({}))),
    ...JSON.parse(current.memo_list(JSON.stringify({ archived: true }))),
  ];
  const paths = exportPaths(all);
  for (const summary of all) {
    const detail: MemoDetail = JSON.parse(current.memo_detail(summary.id));
    const up = detail.archived ? "../" : "";
    let body = linkExported(detail.body, paths, up);
    for (const a of detail.attachments) {
      const name = `images/${a.id}.${extension(a.mime)}`;
      const image = await attachment(detail.id, a.id, "original").catch(() => null);
      if (!image) continue;
      files.push({ name, bytes: image.bytes });
      body = body.replaceAll(`attachment:${a.id}`, up + name);
    }
    files.push({ name: paths.get(detail.id)!, bytes: encoder.encode(body) });
  }
  return zip(files);
}

// ---- RPC ----

const methods = {
  session,
  login,
  unlock,
  register,
  recover,
  finishOnboarding,
  logout,
  lock,
  passwordProblem: async (password: string) => {
    await ready;
    return wasm.password_problem(password) ?? null;
  },

  sync: () => runSync(),
  poke: () => scheduleSync(0),
  list: (filter: MemoFilter): MemoSummary[] => JSON.parse(unlocked().memo_list(JSON.stringify(filter))),
  tags: (): TagCount[] => JSON.parse(unlocked().tag_counts()),
  activity: (): number[] => JSON.parse(unlocked().activity()),
  stats: (): Stats => JSON.parse(unlocked().stats()),
  savedFilters: (): SavedFilter[] => JSON.parse(unlocked().saved_filters()),
  saveFilter: (name: string, filter: MemoFilter) => {
    const id = unlocked().save_filter(name, JSON.stringify(filter));
    edited();
    return id;
  },
  deleteFilter: (id: string) => {
    unlocked().delete_filter(id);
    edited();
  },
  memo: (id: string): MemoDetail => JSON.parse(unlocked().memo_detail(id)),
  render: async (body: string) => {
    await ready;
    return wasm.render_markdown(body);
  },
  attachment,

  create: (body: string) => {
    const id = unlocked().create_memo(body, now());
    edited();
    return id;
  },
  edit: (id: string, body: string, prune: boolean) => {
    unlocked().edit_memo(id, body, now(), prune);
    edited();
  },
  toggleTask: (id: string, offset: number) => {
    const done = unlocked().toggle_task(id, offset, now());
    if (done) edited();
    return done;
  },
  remove: (id: string) => {
    unlocked().delete_memo(id);
    edited();
  },
  setState: (id: string, pinned: boolean, archived: boolean) => {
    unlocked().set_memo_state(id, pinned, archived);
    edited();
  },
  resolve: (id: string, body: string) => {
    unlocked().resolve_conflict(id, body, now());
    edited();
  },
  saveAsCopy: (id: string) => {
    const copy = unlocked().save_as_copy(id, now());
    edited();
    return copy;
  },
  attachImage,

  revisions: (id: string) => http.json<RevisionInfo[]>("GET", `/memos/${id}/revisions`),
  revision: async (id: string, version: number) =>
    unlocked().open_revision(id, await http.text("GET", `/memos/${id}/revisions/${version}`)),
  restoreRevision: async (id: string, version: number) => {
    unlocked().restore_revision(id, await http.text("GET", `/memos/${id}/revisions/${version}`), now());
    edited();
  },

  lookupUser,
  share,
  setPermission: async (memoId: string, userId: string, permission: Permission) => {
    await http.json("PATCH", `/memos/${memoId}/members/${userId}`, { permission });
    await runSync();
  },
  revoke,
  leave: async (memoId: string) => {
    await http.call("DELETE", `/memos/${memoId}/members/me`);
    await runSync();
  },
  verifyContact: (userId: string) => {
    unlocked().verify_contact(userId);
    edited();
  },

  me: async () => (me = await http.json<Me>("GET", "/me")),
  devices: () => http.json<Device[]>("GET", "/devices"),
  removeDevice: (id: string) => http.call("DELETE", `/devices/${id}`).then(() => undefined),
  changePassword: async (current: string, next: string, revokeOthers: boolean) => {
    const request = unlocked().password_change(current, await prelogin(), next, revokeOthers);
    await http.text("POST", "/auth/password", request);
  },
  deleteAccount: async (password: string) => {
    const request = unlocked().delete_account(password, await prelogin());
    await http.text("DELETE", "/me", request);
    signedOut();
  },
  exportAll,
};

export type Methods = typeof methods;

scope.addEventListener("message", async (event: MessageEvent) => {
  const { id, method, args } = event.data as { id: number; method: keyof Methods; args: unknown[] };
  try {
    const fn = methods[method] as (...args: unknown[]) => unknown;
    if (!fn) throw new BackendError("bad_method", String(method));
    const result = await fn(...args);
    scope.postMessage({ id, result });
  } catch (e) {
    const error = e as { name?: string; code?: string; message?: string; status?: number };
    scope.postMessage({
      id,
      error: { code: error.code ?? error.name ?? "error", message: error.message ?? String(e), status: error.status ?? 0 },
    });
  }
});
