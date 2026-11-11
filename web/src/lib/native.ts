// The desktop app's backend (`desktop/`): memo-core with its SQLite store runs on the Rust side of
// the Tauri app, which also does the HTTP and keeps the keys in the OS keyring. This drives it over
// one IPC command, schedules sync (`docs/sync.md#when-sync-runs`) and keeps the account. Unlike the
// browser it works offline: everything is local, and sync catches up when the server is reachable.
//
// The calls are in `desktop/src/calls.rs`; `core` forwards to `crates/native`. In debug builds,
// `memo-desktop --serve` answers the same calls over HTTP (`?ipc=http`) for the end-to-end test.

import { exportPaths, extension, linkExported } from "./exported";
import { prepareImage } from "./images";
import type {
  AttachedImage,
  AttachmentKind,
  Backend,
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
  Trust,
  UserInfo,
} from "./types";
import { BackendError } from "./types";
import { zip } from "./zip";

const EDIT_DELAY = 2_000;
const PERIODIC = 5 * 60_000;
const MAX_BACKOFF = 5 * 60_000;
/** Downloaded image blobs kept on the device (`docs/sync.md#attachments-transfer`). */
const BLOB_CACHE = 2 ** 30;

type Transport = (name: string, body: Uint8Array) => Promise<Uint8Array>;

/** The Tauri window, or `memo-desktop --serve` (`?ipc=http`); `null` in a plain browser. */
export function nativeTransport(): Transport | null {
  // What `@tauri-apps/api`'s `invoke` calls; using it directly saves the dependency.
  const tauri = (window as { __TAURI_INTERNALS__?: { invoke: (...args: unknown[]) => Promise<ArrayBuffer> } })
    .__TAURI_INTERNALS__;
  if (tauri) {
    return async (name, body) => {
      try {
        return new Uint8Array(await tauri.invoke("call", body, { headers: { "memo-call": name } }));
      } catch (e) {
        throw nativeError(String(e));
      }
    };
  }
  if (new URLSearchParams(location.search).get("ipc") === "http") {
    return async (name, body) => {
      const response = await fetch("/ipc", { method: "POST", headers: { "memo-call": name }, body: body as BodyInit });
      if (!response.ok) throw nativeError(await response.text());
      return new Uint8Array(await response.arrayBuffer());
    };
  }
  return null;
}

/** `network: …` and `bad_url: …` from `calls.rs` become those codes. */
function nativeError(message: string): BackendError {
  const [code, rest] = message.split(/: (.*)/s);
  if (code === "network") return new BackendError("network", "The server cannot be reached.");
  if (code === "bad_url") return new BackendError("bad_url", `This is not a server address (${rest}).`);
  return new BackendError("native", message);
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** A big-endian `u32` length, the JSON, then the bytes. */
function frame(json: unknown, data?: Uint8Array): Uint8Array {
  const text = encoder.encode(JSON.stringify(json));
  const out = new Uint8Array(4 + text.length + (data?.length ?? 0));
  new DataView(out.buffer).setUint32(0, text.length);
  out.set(text, 4);
  if (data) out.set(data, 4 + text.length);
  return out;
}

/** `https://memo.example.org/` → `https://memo.example.org`; no scheme means HTTPS. */
function serverUrl(input: string): string {
  let url = input.trim().replace(/\/+$/, "");
  if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
  try {
    return new URL(url).origin;
  } catch {
    throw new BackendError("bad_url", "This is not a server address.");
  }
}

/** The account file (`account.json`): who is signed in, without the secrets. */
interface Account {
  server: string;
  username: string;
  user_id: string;
  /** The last `GET /me`, for offline starts. */
  me: Me | null;
}

/** What `crates/native` returns when it opens an account; `secrets` go to the keyring. */
interface Opened {
  user_id: string;
  fingerprint: string;
  secrets: unknown;
}

/** The keyring entry. */
interface Stored {
  token: string;
  secrets: unknown;
}

interface Pending {
  server: string;
  username: string;
  token: string;
  opened: Opened;
}

export function nativeBackend(transport: Transport): Backend {
  const listeners: ((event: BackendEvent) => void)[] = [];
  let account: Account | null = null;
  /** Set while unlocked. */
  let token: string | null = null;
  let fingerprint = "";
  /** After register or recover: open, but the user has not confirmed the recovery key yet. */
  let onboarding: Pending | null = null;
  let deviceName = "Desktop";

  function emit(event: BackendEvent) {
    for (const listener of listeners) listener(event);
  }

  const now = () => Date.now();
  const device = () => ({ name: deviceName, kind: "desktop" });

  async function ipc(name: string, body: Uint8Array | string = new Uint8Array()): Promise<Uint8Array> {
    return transport(name, typeof body === "string" ? encoder.encode(body) : body);
  }

  async function ipcJson<T>(name: string, body?: Uint8Array | string): Promise<T> {
    return JSON.parse(decoder.decode(await ipc(name, body)));
  }

  /** A `crates/native` call: its JSON reply and bytes. */
  async function core<T = unknown>(
    call: string,
    args: Record<string, unknown> = {},
    data?: Uint8Array,
  ): Promise<{ value: T; data: Uint8Array | null }> {
    const out = await ipc("core", frame({ call, ...args }, data));
    const len = new DataView(out.buffer, out.byteOffset).getUint32(1);
    const value = JSON.parse(decoder.decode(out.subarray(5, 5 + len)));
    if (out[0] !== 0) throw new BackendError(value.code, value.message);
    const rest = out.subarray(5 + len);
    return { value, data: rest.length ? rest : null };
  }

  const coreValue = async <T>(call: string, args: Record<string, unknown> = {}, data?: Uint8Array) =>
    (await core<T>(call, args, data)).value;

  // ---- HTTP ----

  /** The API on `server` (default: the account's), with this device's token unless `auth` is false. */
  async function http(
    method: string,
    path: string,
    body?: unknown,
    options: { server?: string; auth?: boolean } = {},
  ): Promise<Uint8Array> {
    const server = options.server ?? account?.server;
    if (!server) throw new BackendError("locked", "Sign in first.");
    const raw = body instanceof Uint8Array;
    const request = {
      method,
      url: `${server}/api/v1${path}`,
      token: options.auth === false ? null : token,
      type: body === undefined ? null : raw ? "application/octet-stream" : "application/json",
    };
    const data = body === undefined ? undefined : raw ? body : encoder.encode(JSON.stringify(body));
    const out = await ipc("http", frame(request, data));
    const status = new DataView(out.buffer, out.byteOffset).getUint32(0);
    const bytes = out.subarray(4);
    if (status < 200 || status >= 300) {
      let code = `http_${status}`;
      let message = `HTTP ${status}`;
      try {
        const error = JSON.parse(decoder.decode(bytes)).error;
        code = error.code;
        message = error.message;
      } catch {
        // Not an API error body (e.g. from the reverse proxy).
      }
      throw new BackendError(code, message, status);
    }
    return bytes;
  }

  async function json<T>(method: string, path: string, body?: unknown, options?: { server?: string; auth?: boolean }) {
    const bytes = await http(method, path, body, options);
    return (bytes.length ? JSON.parse(decoder.decode(bytes)) : undefined) as T;
  }

  function api(): string {
    if (!token) throw new BackendError("locked", "Unlock first.");
    return token;
  }

  // ---- Session ----

  /** Opens the account saved on this device, if its keys are in the keyring. */
  const ready = (async () => {
    deviceName = (await ipcJson<{ device: string }>("info")).device;
    account = await ipcJson<Account | null>("account_get");
    if (!account) return;
    const stored = await ipcJson<string | null | { unavailable: string }>("secrets_get");
    if (typeof stored !== "string") return;
    try {
      const saved: Stored = JSON.parse(stored);
      const opened = await coreValue<Opened>("open", { secrets: saved.secrets });
      token = saved.token;
      fingerprint = opened.fingerprint;
      void runSync();
    } catch {
      // Unreadable keys: unlock with the password.
    }
  })();

  function currentSession(): Session {
    if (!account) return { state: "signed_out" };
    if (!token) return { state: "locked", username: account.username };
    const me: Me = account.me ?? {
      user_id: account.user_id,
      username: account.username,
      role: "user",
      public_key: "",
      quota_bytes: 0,
      used_bytes: 0,
      created_at: 0,
    };
    return { state: "unlocked", me, fingerprint };
  }

  async function signIn({ server, username, token: next, opened }: Pending) {
    token = next;
    fingerprint = opened.fingerprint;
    account = { server, username: username.trim().toLowerCase(), user_id: opened.user_id, me: null };
    account.me = await json<Me>("GET", "/me").catch(() => null);
    if (account.me) account.username = account.me.username;
    await ipc("account_set", JSON.stringify(account));
    const stored: Stored = { token: next, secrets: opened.secrets };
    // Without a keyring the keys stay in memory, and the next start asks for the password.
    await ipc("secrets_set", JSON.stringify(stored)).catch(() => undefined);
    emit({ type: "session", session: currentSession() });
    void runSync();
  }

  async function login(username: string, password: string, server?: string) {
    const base = server ? serverUrl(server) : account?.server;
    if (!base) throw new BackendError("bad_url", "Enter the server's address.");
    const prelogin = await json("POST", "/auth/prelogin", { username: username.trim() }, { server: base, auth: false });
    const request = await coreValue("login_start", { password, prelogin, username, device: device() });
    const response = await json<{ token: string }>("POST", "/auth/login", request, { server: base, auth: false });
    const opened = await coreValue<Opened>("login_finish", { response });
    await signIn({ server: base, username, token: response.token, opened });
  }

  async function register(username: string, password: string, invite: string, server?: string) {
    if (!server) throw new BackendError("bad_url", "Enter the server's address.");
    const base = serverUrl(server);
    const started = await coreValue<{ request: unknown; recovery_key: string }>("register_start", {
      password,
      username,
      invite,
      device: device(),
    });
    const response = await json<{ token: string }>("POST", "/auth/register", started.request, {
      server: base,
      auth: false,
    });
    const opened = await coreValue<Opened>("register_finish");
    onboarding = { server: base, username, token: response.token, opened };
    return started.recovery_key;
  }

  async function recover(username: string, recoveryKey: string, newPassword: string, server?: string) {
    if (!server) throw new BackendError("bad_url", "Enter the server's address.");
    const base = serverUrl(server);
    const options = { server: base, auth: false };
    const request = await coreValue("recover_start", { recovery_key: recoveryKey, username });
    const response = await json("POST", "/auth/recover", request, options);
    const completed = await coreValue<{ request: unknown; recovery_key: string }>("recover_complete", {
      response,
      new_password: newPassword,
      device: device(),
    });
    const done = await json<{ token: string }>("POST", "/auth/recover/complete", completed.request, options);
    const opened = await coreValue<Opened>("recover_finish");
    onboarding = { server: base, username, token: done.token, opened };
    return completed.recovery_key;
  }

  /** Forgets the account and deletes this device's copy (sign-out, or the device was signed out). */
  async function wipe() {
    const userId = account?.user_id;
    clearTimeout(syncTimer);
    token = null;
    onboarding = null;
    account = null;
    try {
      await (userId ? core("forget", { user_id: userId }) : core("close"));
    } finally {
      await ipc("secrets_delete").catch(() => undefined);
      await ipc("account_delete");
      status = { syncing: false, last: null, error: null, unsynced: false };
      emit({ type: "sync", status });
      emit({ type: "session", session: { state: "signed_out" } });
    }
  }

  /** Signs this device out on the server, if it can be reached. */
  async function serverLogout() {
    if (token) await http("POST", "/auth/logout").catch(() => undefined);
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
    if (!token) return;
    clearTimeout(syncTimer);
    syncTimer = setTimeout(() => void runSync(), delay);
  }

  /** Runs a sync now, or once more after the one in progress. Errors go to the status. */
  function runSync(): Promise<void> {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      do {
        again = false;
        await syncRound();
      } while (again && token);
    })().finally(() => (running = null));
    return running;
  }

  async function syncRound() {
    if (!token) return;
    clearTimeout(syncTimer);
    setStatus({ syncing: true });
    try {
      const events = await syncOnce();
      failures = 0;
      setStatus({ last: now(), error: null });
      if (events.length) emit({ type: "events", events });
      emit({ type: "changed" });
      scheduleSync(PERIODIC);
    } catch (e) {
      if (e instanceof BackendError && e.code === "unauthenticated") {
        // This device was signed out (or the account deleted) elsewhere: its copy goes too.
        await wipe();
        return;
      }
      failures++;
      setStatus({ error: e instanceof Error ? e.message : String(e) });
      scheduleSync(Math.min(MAX_BACKOFF, 5_000 * 2 ** Math.min(failures - 1, 10)));
    } finally {
      const unsynced = token ? await coreValue<boolean>("has_unsynced").catch(() => false) : false;
      setStatus({ syncing: false, unsynced });
    }
  }

  async function syncOnce(): Promise<SyncEvent[]> {
    let body: unknown = await coreValue("sync_start", { now: now() });
    let events: SyncEvent[];
    try {
      while (body !== null) {
        const response = await json("POST", "/sync", body);
        body = await coreValue("sync_handle", { response });
      }
    } finally {
      events = await coreValue<SyncEvent[]>("sync_finish");
    }
    const uploads = await coreValue<{ memo_id: string; attachment_id: string; kind: AttachmentKind }[]>(
      "pending_uploads",
    );
    for (const upload of uploads) {
      const ids = { attachment_id: upload.attachment_id, kind: upload.kind };
      const blob = (await core("blob", ids)).data;
      if (!blob) continue;
      try {
        await http("PUT", `/memos/${upload.memo_id}/attachments/${upload.attachment_id}/${upload.kind}`, blob);
        await core("upload_done", ids);
      } catch (e) {
        // Too large or over quota: retrying every sync will not help; the image stays local.
        if (e instanceof BackendError && (e.code === "too_large" || e.code === "quota_exceeded")) {
          events.push({ type: "push_rejected", memo_id: upload.memo_id, reason: "quota_exceeded" });
          continue;
        }
        throw e;
      }
    }
    await core("trim_blobs", { budget: BLOB_CACHE });
    return events;
  }

  /** A local change: sync soon, and tell the page. */
  async function edited() {
    emit({ type: "changed" });
    setStatus({ unsynced: await coreValue<boolean>("has_unsynced") });
    // A round already running pushed what was dirty when it started: run one more after it.
    if (running) again = true;
    else scheduleSync(EDIT_DELAY);
  }

  window.addEventListener("online", () => scheduleSync(0));

  // ---- Memos ----

  const downloads = new Map<string, Promise<void>>();

  async function attachmentBytes(memoId: string, attachmentId: string, kind: AttachmentKind) {
    const ids = { memo_id: memoId, attachment_id: attachmentId, kind };
    const local = (await core("attachment", ids)).data;
    if (local) return local;
    const key = `${attachmentId}/${kind}`;
    let download = downloads.get(key);
    if (!download) {
      download = http("GET", `/attachments/${attachmentId}/${kind}`)
        .then((envelope) => core("put_blob", { attachment_id: attachmentId, kind }, envelope))
        .then(() => undefined)
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
    const bytes = (await core("attachment", ids)).data;
    if (!bytes) throw new BackendError("decrypt", "The image cannot be decrypted.");
    return bytes;
  }

  async function attachment(memoId: string, attachmentId: string, kind: AttachmentKind) {
    const bytes = await attachmentBytes(memoId, attachmentId, kind);
    if (!bytes) return null;
    const detail = await coreValue<MemoDetail>("memo_detail", { memo_id: memoId });
    const view = detail.attachments.find((a) => a.id === attachmentId);
    const mime = kind === "thumbnail" ? view?.thumb?.mime : view?.mime;
    const renderable = mime !== undefined && (await coreValue<boolean>("renderable", { mime }, bytes));
    return { bytes, renderable };
  }

  async function clean(bytes: Uint8Array) {
    const { value, data } = await core<{ mime: string; width: number; height: number; orientation: number }>(
      "clean_image",
      {},
      bytes,
    );
    return { ...value, bytes: data ?? new Uint8Array() };
  }

  async function attachImage(memoId: string, file: Blob, maxSide: number | null): Promise<AttachedImage> {
    const image = await prepareImage(file, maxSide, clean);
    const thumb = image.thumb;
    const data = new Uint8Array(image.bytes.length + (thumb?.bytes.length ?? 0));
    data.set(image.bytes);
    if (thumb) data.set(thumb.bytes, image.bytes.length);
    const id = await coreValue<string>(
      "add_attachment",
      {
        memo_id: memoId,
        mime: image.mime,
        width: image.width,
        height: image.height,
        size: image.bytes.length,
        thumb: thumb ? { mime: thumb.mime, width: thumb.width, height: thumb.height } : null,
      },
      data,
    );
    await edited();
    return { id, width: image.width, height: image.height };
  }

  /** A local change through memo-core. */
  async function change<T>(call: string, args: Record<string, unknown>): Promise<T> {
    const value = await coreValue<T>(call, args);
    await edited();
    return value;
  }

  // ---- Sharing ----

  async function share(memoId: string, user: UserInfo, permission: Permission) {
    // The memo must be on the server.
    await runSync();
    const request = await coreValue("prepare_share", { memo_id: memoId, user, permission, now: now() });
    await http("POST", `/memos/${memoId}/members`, request);
    await runSync();
  }

  async function revoke(memoId: string, userIds: string[]) {
    for (let attempt = 0; ; attempt++) {
      await runSync();
      const request = await coreValue("prepare_revoke", { memo_id: memoId, user_ids: userIds });
      try {
        const response = await json("POST", `/memos/${memoId}/revoke`, request);
        await core("apply_revoke", { memo_id: memoId, response });
        break;
      } catch (e) {
        // Someone wrote in the meantime: sync and re-encrypt.
        if (!(e instanceof BackendError && e.code === "conflict" && attempt < 2)) throw e;
      }
    }
    await runSync();
  }

  // ---- Account ----

  async function prelogin() {
    if (!account) throw new BackendError("locked", "Sign in first.");
    return json("POST", "/auth/prelogin", { username: account.username }, { auth: false });
  }

  async function revision(id: string, version: number) {
    return json("GET", `/memos/${id}/revisions/${version}`);
  }

  // ---- Export ----

  async function exportAll(): Promise<Uint8Array> {
    const files: { name: string; bytes: Uint8Array }[] = [];
    const all = [
      ...(await coreValue<MemoSummary[]>("memo_list", { filter: {} })),
      ...(await coreValue<MemoSummary[]>("memo_list", { filter: { archived: true } })),
    ];
    const paths = exportPaths(all);
    for (const summary of all) {
      const detail = await coreValue<MemoDetail>("memo_detail", { memo_id: summary.id });
      const up = detail.archived ? "../" : "";
      let body = linkExported(detail.body, paths, up);
      for (const a of detail.attachments) {
        const name = `images/${a.id}.${extension(a.mime)}`;
        const bytes = await attachmentBytes(detail.id, a.id, "original").catch(() => null);
        if (!bytes) continue;
        files.push({ name, bytes });
        body = body.replaceAll(`attachment:${a.id}`, up + name);
      }
      files.push({ name: paths.get(detail.id)!, bytes: encoder.encode(body) });
    }
    return zip(files);
  }

  return {
    native: true,
    on: (listener) => void listeners.push(listener),
    session: async () => {
      await ready;
      return currentSession();
    },
    login: async (username, password, server) => {
      await ready;
      await login(username, password, server);
    },
    unlock: async (password) => {
      await ready;
      if (!account) throw new BackendError("locked", "Sign in first.");
      await login(account.username, password, account.server);
    },
    register: async (username, password, invite, server) => {
      await ready;
      return register(username, password, invite, server);
    },
    recover: async (username, recoveryKey, newPassword, server) => {
      await ready;
      return recover(username, recoveryKey, newPassword, server);
    },
    finishOnboarding: async () => {
      if (!onboarding) throw new BackendError("not_found", "Nothing to finish.");
      const pending = onboarding;
      onboarding = null;
      await signIn(pending);
    },
    logout: async () => {
      await serverLogout();
      await wipe();
    },
    lock: async () => {
      // The memos stay; unlocking signs in again (`docs/crypto.md`).
      await serverLogout();
      clearTimeout(syncTimer);
      token = null;
      await core("close");
      await ipc("secrets_delete").catch(() => undefined);
      emit({ type: "session", session: currentSession() });
    },
    passwordProblem: (password) => coreValue<string | null>("password_problem", { password }),

    sync: () => runSync(),
    poke: () => scheduleSync(0),
    list: (filter: MemoFilter) => coreValue<MemoSummary[]>("memo_list", { filter }),
    tags: () => coreValue<TagCount[]>("tag_counts"),
    activity: () => coreValue<number[]>("activity"),
    stats: () => coreValue<Stats>("stats"),
    savedFilters: () => coreValue<SavedFilter[]>("saved_filters"),
    saveFilter: (name, filter) => change<string>("save_filter", { name, filter }),
    deleteFilter: (id) => change("delete_filter", { filter_id: id }),
    memo: (id) => coreValue<MemoDetail>("memo_detail", { memo_id: id }),
    render: (body) => coreValue<string>("render_html", { body }),
    attachment,

    create: (body) => change<string>("create", { body, now: now() }),
    edit: (id, body, prune) => change("edit", { memo_id: id, body, now: now(), prune }),
    toggleTask: async (id, offset) => {
      const done = await coreValue<boolean>("toggle_task", { memo_id: id, offset, now: now() });
      if (done) await edited();
      return done;
    },
    remove: (id) => change("delete", { memo_id: id }),
    setState: (id, pinned, archived) => change("set_state", { memo_id: id, pinned, archived }),
    resolve: (id, body) => change("resolve", { memo_id: id, body, now: now() }),
    saveAsCopy: (id) => change<string>("save_as_copy", { memo_id: id, now: now() }),
    attachImage,

    revisions: (id) => json<RevisionInfo[]>("GET", `/memos/${id}/revisions`),
    revision: async (id, version) =>
      coreValue<string>("open_revision", { memo_id: id, revision: await revision(id, version) }),
    restoreRevision: async (id, version) =>
      change("restore_revision", { memo_id: id, revision: await revision(id, version), now: now() }),

    lookupUser: async (username) => {
      const user = await json<UserInfo>("GET", `/users/lookup?username=${encodeURIComponent(username.trim())}`);
      const trust = await coreValue<{ trust: Trust; fingerprint: string }>("contact_trust", { user });
      return { ...user, ...trust };
    },
    share,
    setPermission: async (memoId, userId, permission) => {
      await json("PATCH", `/memos/${memoId}/members/${userId}`, { permission });
      await runSync();
    },
    revoke,
    leave: async (memoId) => {
      await http("DELETE", `/memos/${memoId}/members/me`);
      await runSync();
    },
    verifyContact: (userId) => change("verify_contact", { user_id: userId }),

    me: async () => {
      api();
      try {
        const me = await json<Me>("GET", "/me");
        if (account) {
          account.me = me;
          await ipc("account_set", JSON.stringify(account));
        }
        return me;
      } catch (e) {
        // Offline: what this device saw last.
        if (e instanceof BackendError && e.code === "network" && account?.me) return account.me;
        throw e;
      }
    },
    devices: () => json<Device[]>("GET", "/devices"),
    removeDevice: async (id) => {
      await http("DELETE", `/devices/${id}`);
    },
    changePassword: async (current, next, revokeOthers) => {
      const request = await coreValue("password_change", {
        current_password: current,
        prelogin: await prelogin(),
        new_password: next,
        revoke_other_devices: revokeOthers,
      });
      await http("POST", "/auth/password", request);
    },
    deleteAccount: async (password) => {
      const request = await coreValue("delete_account", { password, prelogin: await prelogin() });
      await http("DELETE", "/me", request);
      await wipe();
    },
    exportAll,
    saveFile: async (bytes, name) => ipcJson<string>("save", frame({ name }, bytes)),
    openLink: (url) => void ipc("open", url).catch(() => undefined),
  };
}
