# Server

`memo-server` is a single binary: axum + SQLite, with the built web app (`web/dist`) embedded by its build script. It
never decrypts anything. Its jobs are authentication, access control, ordering, storage, quotas and
rate limits.

## Database (SQLite)

Connection settings: `journal_mode=WAL`, `synchronous=FULL`, `foreign_keys=ON`, `busy_timeout=5000`.
All writes go through a single writer connection; reads use a small pool. Schema migrations are
embedded in the binary and run at startup.

UUIDs are stored as 16-byte `BLOB`s, times as Unix milliseconds. The schema below is
`crates/server/migrations/0001_init.sql` (indexes omitted here).

```sql
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,                    -- 'epoch', 'schema_version'
  value BLOB NOT NULL
);

CREATE TABLE users (
  id                       BLOB PRIMARY KEY,
  username                 TEXT NOT NULL UNIQUE,
  role                     TEXT NOT NULL CHECK (role IN ('admin', 'user')),
  disabled                 INTEGER NOT NULL DEFAULT 0,
  auth_hash                TEXT NOT NULL,      -- Argon2id PHC string of auth_key
  kdf_salt                 BLOB NOT NULL,
  kdf_m                    INTEGER NOT NULL,
  kdf_t                    INTEGER NOT NULL,
  kdf_p                    INTEGER NOT NULL,
  recovery_hash            TEXT NOT NULL,      -- Argon2id PHC string of recovery_auth_key
  public_key               BLOB NOT NULL,
  enc_account_key          BLOB NOT NULL,
  enc_account_key_recovery BLOB NOT NULL,
  enc_private_key          BLOB NOT NULL,
  quota_bytes              INTEGER,            -- NULL = server default
  purged_through           INTEGER NOT NULL DEFAULT 0,
  created_at               INTEGER NOT NULL
);

CREATE TABLE invites (
  code_hash  BLOB PRIMARY KEY,                 -- SHA-256 of the invite code
  role       TEXT NOT NULL DEFAULT 'user',
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
  token_hash   BLOB NOT NULL UNIQUE,           -- SHA-256 of the token
  created_at   INTEGER NOT NULL,
  last_seen_at INTEGER NOT NULL,
  revoked_at   INTEGER
);

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
  ciphertext  BLOB,                            -- NULL once deleted
  deleted     INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL,
  updated_at  INTEGER NOT NULL,
  updated_by  BLOB,
  deleted_at  INTEGER                          -- tombstone age, for purging
);

CREATE TABLE memo_revisions (                  -- includes the current version
  memo_id     BLOB NOT NULL REFERENCES memos(id) ON DELETE CASCADE,
  version     INTEGER NOT NULL,
  key_version INTEGER NOT NULL,
  ciphertext  BLOB NOT NULL,
  updated_by  BLOB,
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (memo_id, version)
);

CREATE TABLE memo_members (                    -- the owner always has a row with permission 'write'
  memo_id    BLOB NOT NULL REFERENCES memos(id) ON DELETE CASCADE,
  user_id    BLOB NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  permission TEXT NOT NULL CHECK (permission IN ('read', 'write')),
  added_at   INTEGER NOT NULL,
  PRIMARY KEY (memo_id, user_id)
);

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
  size         INTEGER,                        -- NULL until the original is uploaded
  sha256       BLOB,
  thumb_size   INTEGER,
  thumb_sha256 BLOB,
  created_at   INTEGER NOT NULL,
  deleted_at   INTEGER,
  purged_at    INTEGER                         -- blob files removed; no longer counts against the quota
);
CREATE INDEX attachments_memo ON attachments(memo_id);
CREATE INDEX attachments_uploader ON attachments(uploader_id);

CREATE TABLE change_log (
  seq         INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id     BLOB NOT NULL,
  entity_type TEXT NOT NULL CHECK (entity_type IN ('memo', 'access', 'attachment', 'state', 'vault')),
  entity_id   BLOB NOT NULL,                   -- the vault's entity_id is the user id
  changed_at  INTEGER NOT NULL,
  UNIQUE (user_id, entity_type, entity_id)
);
CREATE INDEX change_log_user_seq ON change_log(user_id, seq);
```

A revoked user's `access` row in `change_log` is kept while it has no matching `memo_members` row. The
server renders that as `{"type": "access", "revoked": true}`. When a memo is deleted, its row stays as
a tombstone (`deleted = 1`, no ciphertext) until the tombstone retention period ends.

**Quota:** `used_bytes` = the size of all memo ciphertexts and revisions the user **owns** + the
attachment bytes the user **uploaded** that are not yet purged. It is checked on every write that adds
bytes.

## Storage layout

```
<data_dir>/
  memo.db, memo.db-wal, memo.db-shm
  secret                          32 random bytes, mode 0600, generated on first start
  blobs/<last 2 hex chars of id>/<attachment-id>.orig     (UUIDv7 starts with a timestamp, so the
  blobs/<last 2 hex chars of id>/<attachment-id>.thumb     last characters spread files evenly)
  tmp/
```

`secret` is used to derive fake prelogin salts. Blob uploads are streamed to `tmp/` while SHA-256 is
computed. The file is then fsynced, renamed into `blobs/`, and only then is the database row
committed. A crash can leave orphan files behind, but never a database row pointing at a missing blob.

## Background jobs

Run hourly, and on demand with `memo-server gc`:

- Purge blobs of attachments with `deleted_at` older than `deleted_blob_days`.
- Delete orphan files in `blobs/` that have no database row and are older than 1 day, and anything in
  `tmp/` older than 1 day.
- Purge tombstones and `change_log` rows older than `tombstone_days`, and advance `purged_through`.
  Tombstones are rows for deleted memos and attachments, and `access`/`state` rows of users who are
  no longer members. Memo rows nobody references any more are then deleted.
- Remove revisions beyond `revisions_per_memo` (oldest first; the current version is always kept).
- Remove expired recovery tokens and invites.

## Configuration

`/etc/memo/config.toml`, or the path given with `--config`. Every key can be overridden by an
environment variable, e.g. `MEMO_LISTEN`, `MEMO_LIMITS_DEFAULT_QUOTA`.

```toml
listen          = "127.0.0.1:8080"
public_url      = "https://memo.example.com"   # used for the Origin check and cookies
data_dir        = "/var/lib/memo"
trusted_proxies = ["127.0.0.1/32", "::1/128"]  # only these may set X-Forwarded-For

[limits]
default_quota  = "1GiB"
max_memo       = "1MiB"
max_attachment = "25MiB"
max_thumbnail  = "1MiB"

[retention]
tombstone_days     = 90
deleted_blob_days  = 30
revisions_per_memo = 100        # 0 = keep all
```

## Web app

`build.rs` embeds `web/dist` (or `$MEMO_WEB_DIST`) when it exists; otherwise the server is API-only
and answers other paths with a hint. Files under `assets/` have content hashes in their names and
are served with `Cache-Control: public, max-age=31536000, immutable`; everything else with
`no-cache` and an `ETag`. Paths without a file extension serve `index.html` (the app routes with the
URL fragment). `invite create` also prints a `<public_url>/#/invite/<code>` link when the app is built in.

## HTTP hardening

Responses for the embedded web app:

```
Content-Security-Policy: default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self';
                         img-src 'self' blob:; connect-src 'self'; object-src 'none';
                         base-uri 'none'; frame-ancestors 'none'; form-action 'self'
X-Content-Type-Options: nosniff
Referrer-Policy: no-referrer
Cross-Origin-Opener-Policy: same-origin
```

HSTS is configured at the reverse proxy. Logs record method, path, status, latency, device ID and
client IP. They never record request bodies, tokens, cookies or `auth_key`s.

## Admin CLI

```
memo-server serve [--config <path>]
memo-server invite create [--role user|admin] [--expires 7d]    # prints the code once
memo-server invite list                    # ids are the first 8 hex chars of the code's hash
memo-server invite revoke <id>
memo-server user list
memo-server user disable <username>        # blocks login and revokes devices; shared memos stay readable for members
memo-server user enable <username>
memo-server user set-quota <username> <size|default>
memo-server user delete <username>         # same effect as DELETE /me
memo-server backup <dir>                   # safe while the server runs
memo-server restore <dir>                  # stop the server first
memo-server gc
```

Setting up a new server: run `memo-server invite create --role admin` and register with that code.
v1 has no admin web UI. The `admin` role is reserved for one later.

An admin **cannot** reset passwords or read memos. The recovery key is the only way back into an account.

## Backups

`memo-server backup <dir>` writes a consistent database snapshot (`VACUUM INTO`), copies `secret`,
and hard-links or copies `blobs/`. All memo content and blobs are ciphertext, so backups can be stored
off-site without encrypting them again (metadata is still visible, see the threat model).

`memo-server restore <dir>` puts a backup in place and **generates a new `epoch`**, so every client
runs a reset sync and pushes back any data that is newer than the backup (see
[sync.md](sync.md#reset)). Always restore with this command rather than by copying files.

## Deployment

The server listens on localhost behind your reverse proxy. The proxy's body size limit must allow the
largest attachment.

Caddy:
```
memo.example.com {
    request_body {
        max_size 32MB
    }
    reverse_proxy 127.0.0.1:8080
}
```

nginx:
```nginx
client_max_body_size 32m;
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header Host $host;
    proxy_buffering off;          # needed for the later SSE endpoint
}
```

systemd:
```ini
[Unit]
Description=memo server
After=network.target

[Service]
ExecStart=/usr/local/bin/memo-server serve --config /etc/memo/config.toml
User=memo
StateDirectory=memo
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/memo

[Install]
WantedBy=multi-user.target
```
