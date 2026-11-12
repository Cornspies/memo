// The read models from `memo_core::sync::view` and the API types the UI sees, plus the `Backend`
// the UI talks to. The browser's backend runs memo-core (WASM) in a worker (`backend.ts`); the
// desktop app's runs it natively behind Tauri IPC (`native.ts`).

export type Permission = "read" | "write";
export type UndecodedReason = "missing_key" | "undecryptable" | "newer_format";
export type PushError = "forbidden" | "invalid" | "quota_exceeded";
export type Trust = "me" | "verified" | "pinned" | "not_pinned" | "changed";
export type AttachmentKind = "original" | "thumbnail";

export interface MemoSummary {
  id: string;
  owner_id: string;
  title: string;
  preview: string;
  tags: string[];
  pinned: boolean;
  archived: boolean;
  shared: boolean;
  writable: boolean;
  created_at: number;
  updated_at: number;
  dirty: boolean;
  conflict: boolean;
  undecoded: UndecodedReason | null;
  push_error: PushError | null;
  images: number;
}

export interface MemberView {
  user_id: string;
  username: string;
  permission: Permission;
  trust: Trust;
  fingerprint: string;
  /** On your own memo: the server lists them, but you never shared it with them. */
  unconfirmed: boolean;
}

export interface ThumbRef {
  mime: string;
  width: number;
  height: number;
}

export interface AttachmentView {
  id: string;
  mime: string;
  size: number;
  width: number;
  height: number;
  thumb: ThumbRef | null;
  available: boolean;
  thumb_available: boolean;
}

export interface MemoDetail extends MemoSummary {
  body: string;
  edited_at: number;
  server_version: number;
  updated_by: string | null;
  own: boolean;
  members: MemberView[];
  attachments: AttachmentView[];
  conflict_with: { body: string; version: number; edited_at: number } | null;
  /** Memos that link to this one. */
  linked_from: MemoLink[];
}

export interface MemoLink {
  id: string;
  title: string;
}

/** The quick filters: what a memo must contain. */
export type Has = "tasks" | "open_tasks" | "links" | "images" | "code";

export interface MemoFilter {
  query?: string;
  tag?: string | null;
  archived?: boolean;
  has?: Has[];
  /** Created in [created_from, created_before), Unix ms. */
  created_from?: number | null;
  created_before?: number | null;
}

export interface SavedFilter {
  id: string;
  name: string;
  query: string;
  tag: string | null;
  has: Has[];
}

/** Counts over the memo list (the archive only as `archived`). */
export interface Stats {
  memos: number;
  archived: number;
  words: number;
  tags: number;
  images: number;
  open_tasks: number;
  done_tasks: number;
  shared: number;
  since: number | null;
}

export interface TagCount {
  tag: string;
  count: number;
}

export interface Me {
  user_id: string;
  username: string;
  role: "admin" | "user";
  public_key: string;
  quota_bytes: number;
  used_bytes: number;
  created_at: number;
}

export interface Device {
  id: string;
  name: string;
  kind: "web" | "desktop" | "android";
  created_at: number;
  last_seen_at: number;
  current: boolean;
}

export interface UserInfo {
  user_id: string;
  username: string;
  public_key: string;
}

export interface RevisionInfo {
  version: number;
  key_version: number;
  updated_by: string | null;
  updated_at: number;
  size: number;
}

/** `memo_core::sync::SyncEvent`. */
export type SyncEvent =
  | { type: "reset" }
  | { type: "conflict"; memo_id: string }
  | { type: "restored_copy"; original: string; copy: string; reason: "deleted" | "access_revoked" }
  | { type: "delete_rejected"; memo_id: string }
  | { type: "push_rejected"; memo_id: string; reason: PushError }
  | { type: "rollback"; memo_id: string; version: number; highest_seen: number }
  | { type: "server_lost_data"; memo_id: string }
  | { type: "cannot_decrypt"; memo_id: string }
  | { type: "newer_format"; memo_id: string }
  | { type: "key_rejected"; memo_id: string; owner_id: string }
  | { type: "key_changed"; user_id: string }
  | { type: "pin_conflict"; user_id: string }
  | { type: "vault_newer_format" }
  | { type: "vault_undecryptable" };

export type Session =
  | { state: "signed_out" }
  /** Signed in, but the keys are gone (the page was reloaded): the password unlocks again. */
  | { state: "locked"; username: string }
  | { state: "unlocked"; me: Me; fingerprint: string }
  | { state: "offline" };

export interface SyncStatus {
  syncing: boolean;
  /** Unix ms of the last successful sync. */
  last: number | null;
  error: string | null;
  unsynced: boolean;
}

export type BackendEvent =
  | { type: "session"; session: Session }
  | { type: "sync"; status: SyncStatus }
  | { type: "changed" }
  | { type: "events"; events: SyncEvent[] };

/** An image ready to attach: decoded bytes the UI can show right away. */
export interface AttachedImage {
  id: string;
  width: number;
  height: number;
}

export interface Backend {
  /**
   * The desktop app: memos stay on the device and work offline, the keys are kept in the OS
   * keyring, and signing in needs the server's address (`server` below; the browser uses its own).
   */
  readonly native: boolean;
  on(listener: (event: BackendEvent) => void): void;
  session(): Promise<Session>;
  login(username: string, password: string, server?: string): Promise<void>;
  unlock(password: string): Promise<void>;
  /** Returns the recovery key to show once. The session stays pending until `confirmRegistration`. */
  register(username: string, password: string, invite: string, server?: string): Promise<string>;
  /** Returns the new recovery key. */
  recover(username: string, recoveryKey: string, newPassword: string, server?: string): Promise<string>;
  /** Starts using the account after the user confirmed the recovery key. */
  finishOnboarding(): Promise<void>;
  logout(): Promise<void>;
  lock(): Promise<void>;
  passwordProblem(password: string): Promise<string | null>;

  sync(): Promise<void>;
  /** The page came back into view. */
  poke(): void;
  list(filter: MemoFilter): Promise<MemoSummary[]>;
  tags(): Promise<TagCount[]>;
  /** Creation times of the memos in the main list, oldest first. */
  activity(): Promise<number[]>;
  stats(): Promise<Stats>;
  savedFilters(): Promise<SavedFilter[]>;
  /** Saves the filter's query, tag and quick filters under `name`. Returns the new id. */
  saveFilter(name: string, filter: MemoFilter): Promise<string>;
  deleteFilter(id: string): Promise<void>;
  memo(id: string): Promise<MemoDetail>;
  render(body: string): Promise<string>;
  /** Decrypted bytes and whether they may be shown inline, or `null` if not available yet. */
  attachment(
    memoId: string,
    attachmentId: string,
    kind: AttachmentKind,
  ): Promise<{ bytes: Uint8Array; renderable: boolean } | null>;

  create(body: string): Promise<string>;
  edit(id: string, body: string, prune: boolean): Promise<void>;
  toggleTask(id: string, offset: number): Promise<boolean>;
  remove(id: string): Promise<void>;
  setState(id: string, pinned: boolean, archived: boolean): Promise<void>;
  resolve(id: string, body: string): Promise<void>;
  saveAsCopy(id: string): Promise<string>;
  attachImage(memoId: string, file: Blob, maxSide: number | null): Promise<AttachedImage>;

  revisions(id: string): Promise<RevisionInfo[]>;
  revision(id: string, version: number): Promise<string>;
  restoreRevision(id: string, version: number): Promise<void>;

  lookupUser(username: string): Promise<UserInfo & { trust: Trust; fingerprint: string }>;
  share(memoId: string, user: UserInfo, permission: Permission): Promise<void>;
  setPermission(memoId: string, userId: string, permission: Permission): Promise<void>;
  revoke(memoId: string, userIds: string[]): Promise<void>;
  leave(memoId: string): Promise<void>;
  verifyContact(userId: string): Promise<void>;

  me(): Promise<Me>;
  devices(): Promise<Device[]>;
  removeDevice(id: string): Promise<void>;
  changePassword(current: string, next: string, revokeOthers: boolean): Promise<void>;
  deleteAccount(password: string): Promise<void>;
  /** A ZIP of every memo as Markdown, with its images. */
  exportAll(): Promise<Uint8Array>;
  /** Hands a file to the user. Returns where it was saved, if known. */
  saveFile(bytes: Uint8Array, name: string, type: string): Promise<string | null>;
  /** Opens a link from a memo outside the app (desktop only; the browser follows links itself). */
  openLink(url: string): void;
}

/** Errors from the backend: `code` is an API error code or a memo-core error name. */
export class BackendError extends Error {
  constructor(
    public code: string,
    message: string,
    public status = 0,
  ) {
    super(message);
    this.name = "BackendError";
  }
}
