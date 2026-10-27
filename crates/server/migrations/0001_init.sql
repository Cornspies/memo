-- Schema v1. See docs/server.md. UUIDs are 16-byte BLOBs, times Unix milliseconds.

CREATE TABLE meta (
  key   TEXT PRIMARY KEY,                    -- 'epoch', 'schema_version'
  value BLOB NOT NULL
);

CREATE TABLE users (
  id                       BLOB PRIMARY KEY,
  username                 TEXT NOT NULL UNIQUE,
  role                     TEXT NOT NULL CHECK (role IN ('admin', 'user')),
  disabled                 INTEGER NOT NULL DEFAULT 0,
  auth_hash                TEXT NOT NULL,
  kdf_salt                 BLOB NOT NULL,
  kdf_m                    INTEGER NOT NULL,
  kdf_t                    INTEGER NOT NULL,
  kdf_p                    INTEGER NOT NULL,
  recovery_hash            TEXT NOT NULL,
  public_key               BLOB NOT NULL,
  enc_account_key          BLOB NOT NULL,
  enc_account_key_recovery BLOB NOT NULL,
  enc_private_key          BLOB NOT NULL,
  quota_bytes              INTEGER,
  purged_through           INTEGER NOT NULL DEFAULT 0,
  created_at               INTEGER NOT NULL
);

CREATE TABLE invites (
  code_hash  BLOB PRIMARY KEY,
  role       TEXT NOT NULL DEFAULT 'user' CHECK (role IN ('admin', 'user')),
  created_by BLOB REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_by    BLOB REFERENCES users(id) ON DELETE SET NULL,
  used_at    INTEGER
);

CREATE TABLE devices (
  id           BLOB PRIMARY KEY,
  user_id      BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  name         TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK (kind IN ('web', 'desktop', 'android')),
  token_hash   BLOB NOT NULL UNIQUE,
  created_at   INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  revoked_at   INTEGER
);
CREATE INDEX devices_user ON devices(user_id);

CREATE TABLE recovery_tokens (
  token_hash BLOB PRIMARY KEY,
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  expires_at INTEGER NOT NULL
);

CREATE TABLE vaults (
  user_id    BLOB PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  version    INTEGER NOT NULL,
  ciphertext BLOB NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE memos (
  id          BLOB PRIMARY KEY,
  owner_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  version     INTEGER NOT NULL,
  key_version INTEGER NOT NULL,
  ciphertext  BLOB,
  deleted     INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  updated_by  BLOB,
  deleted_at  INTEGER
);
CREATE INDEX memos_owner ON memos(owner_id);

CREATE TABLE memo_revisions (
  memo_id     BLOB NOT NULL REFERENCES memos(id) ON DELETE CASCADE,
  version     INTEGER NOT NULL,
  key_version INTEGER NOT NULL,
  ciphertext  BLOB NOT NULL,
  updated_by  BLOB,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (memo_id, version)
);

CREATE TABLE memo_members (
  memo_id    BLOB NOT NULL REFERENCES memos(id) ON DELETE CASCADE,
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  permission TEXT NOT NULL CHECK (permission IN ('read', 'write')),
  added_at   INTEGER NOT NULL,
  PRIMARY KEY (memo_id, user_id)
);
CREATE INDEX memo_members_user ON memo_members(user_id);

CREATE TABLE memo_keys (
  memo_id     BLOB NOT NULL,
  user_id     BLOB NOT NULL,
  key_version INTEGER NOT NULL,
  wrapped_key BLOB NOT NULL,
  PRIMARY KEY (memo_id, user_id, key_version),
  FOREIGN KEY (memo_id, user_id) REFERENCES memo_members(memo_id, user_id) ON DELETE CASCADE
);

CREATE TABLE memo_state (
  memo_id    BLOB NOT NULL,
  user_id    BLOB NOT NULL,
  pinned     INTEGER NOT NULL DEFAULT 0,
  archived   INTEGER NOT NULL DEFAULT 0,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (memo_id, user_id),
  FOREIGN KEY (memo_id, user_id) REFERENCES memo_members(memo_id, user_id) ON DELETE CASCADE
);

CREATE TABLE attachments (
  id           BLOB PRIMARY KEY,
  memo_id      BLOB NOT NULL REFERENCES memos(id) ON DELETE CASCADE,
  uploader_id  BLOB REFERENCES users(id) ON DELETE SET NULL,
  size         INTEGER,
  sha256       BLOB,
  thumb_size   INTEGER,
  thumb_sha256 BLOB,
  created_at   INTEGER NOT NULL,
  deleted_at   INTEGER,
  purged_at    INTEGER
);
CREATE INDEX attachments_memo ON attachments(memo_id);
CREATE INDEX attachments_uploader ON attachments(uploader_id);

CREATE TABLE change_log (
  seq         INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     BLOB NOT NULL,
  entity_type TEXT NOT NULL CHECK (entity_type IN ('memo', 'access', 'attachment', 'state', 'vault')),
  entity_id   BLOB NOT NULL,
  changed_at  INTEGER NOT NULL,
  UNIQUE (user_id, entity_type, entity_id)
);
CREATE INDEX change_log_user_seq ON change_log(user_id, seq);
CREATE INDEX change_log_entity ON change_log(entity_type, entity_id);
