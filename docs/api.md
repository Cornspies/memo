# HTTP API — v1

Base path: `/api/v1`. TLS is terminated by the reverse proxy. The server only binds to localhost or a
private interface.

## Conventions

- JSON request and response bodies (`Content-Type: application/json`), except attachment blobs.
- Binary fields are base64url without padding. IDs are UUIDv7 strings. Times are Unix milliseconds.
  (See [crypto.md](crypto.md#encoding-conventions).)
- Clients send `Memo-Client: <web|desktop|android>/<version>`. The server answers with `Memo-Api: 1`.
- Errors:
  ```json
  { "error": { "code": "conflict", "message": "human-readable detail" } }
  ```

| HTTP | `code` | When |
|---|---|---|
| 400 | `bad_request` | Malformed JSON, invalid field |
| 401 | `unauthenticated` | Missing, invalid or revoked token/cookie |
| 401 | `invalid_credentials` | Wrong username/`auth_key`/`recovery_auth_key` |
| 403 | `forbidden` | A member, but not allowed (not the owner, or read-only) |
| 403 | `invite_invalid` | Unknown, expired or used invite code |
| 404 | `not_found` | Unknown resource **or one the caller may not see**. Never distinguishes the two |
| 409 | `conflict` | Version mismatch, duplicate username, blob exists with different content |
| 413 | `too_large` | Body over the limit |
| 422 | `quota_exceeded` | Storage quota would be exceeded |
| 422 | `incomplete_keys` | Sharing/revoke request is missing wrapped keys |
| 429 | `rate_limited` | Includes a `Retry-After` header |

## Authentication

- **Native clients:** `Authorization: Bearer <token>`. The token is 32 random bytes in base64url,
  returned once at login. The server stores only its SHA-256.
- **Web:** cookie `memo_session` (`HttpOnly; Secure; SameSite=Strict; Path=/api`). Every request
  authenticated by cookie that is not `GET` must carry an `Origin` header equal to the configured
  public URL. Otherwise the server answers `403 forbidden` (CSRF protection).
- Every authenticated request updates the device's `last_seen_at`, at most once a minute.

Usernames: 3–32 characters of `a-z 0-9 _ . -`, stored lowercase, unique.

### `GET /health` (no auth)
→ `200 {"api": 1, "version": "0.1.0"}`

### `POST /auth/prelogin`
```json
{ "username": "alice" }
```
→ `200 {"kdf": {"salt": "…", "m": 65536, "t": 3, "p": 1}}`, the same shape as in register. For unknown
usernames this returns a fake but stable salt; it never returns 404.

### `POST /auth/register`
```json
{
  "invite_code": "…",
  "user_id": "…", "username": "alice",
  "kdf": { "salt": "…", "m": 65536, "t": 3, "p": 1 },
  "auth_key": "…", "recovery_auth_key": "…",
  "public_key": "…",
  "enc_account_key": "…", "enc_account_key_recovery": "…", "enc_private_key": "…",
  "vault": "…",
  "device": { "name": "Pixel 9", "kind": "android" }
}
```
→ `201 {"device_id": "…", "token": "…"}`. For `kind: "web"` the response sets the cookie and omits `token`.
Errors: `invite_invalid`, `conflict` (username or user_id taken), `bad_request` (KDF parameters below
the minimum).

### `POST /auth/login`
```json
{ "username": "alice", "auth_key": "…", "device": { "name": "Laptop", "kind": "desktop" } }
```
→ `200`
```json
{ "user_id": "…", "device_id": "…", "token": "…",
  "public_key": "…", "enc_account_key": "…", "enc_private_key": "…" }
```

### `POST /auth/logout`
Revokes the current device. → `204`

### `POST /auth/password`
```json
{ "auth_key": "<current>",
  "new": { "kdf": { "salt": "…", "m": 65536, "t": 3, "p": 1 }, "auth_key": "…", "enc_account_key": "…" },
  "revoke_other_devices": true }
```
→ `204`

### `POST /auth/recover`
```json
{ "username": "alice", "recovery_auth_key": "…" }
```
→ `200`
```json
{ "user_id": "…", "recovery_token": "…", "enc_account_key_recovery": "…",
  "public_key": "…", "enc_private_key": "…" }
```
The token is single-use and valid for 10 minutes.

### `POST /auth/recover/complete`
```json
{ "recovery_token": "…",
  "kdf": { "salt": "…", "m": 65536, "t": 3, "p": 1 }, "auth_key": "…", "enc_account_key": "…",
  "recovery_auth_key": "…", "enc_account_key_recovery": "…",
  "device": { "name": "Laptop", "kind": "desktop" } }
```
Revokes **all** existing devices and replaces the password and recovery material.
→ `200 {"device_id": "…", "token": "…"}`

## Account

### `GET /me`
→ `200 {"user_id", "username", "role", "public_key", "quota_bytes", "used_bytes", "created_at"}`

### `GET /devices`
→ `200 [{"id", "name", "kind", "created_at", "last_seen_at", "current": true}]`

### `DELETE /devices/{id}`
→ `204`. The device's next request returns `401`, and its client wipes local data.

### `DELETE /me`
```json
{ "auth_key": "…" }
```
Deletes the account: owned memos are deleted (members lose access), the user is removed from every
memo shared with them, and all their attachments are purged. → `204`

Export is a client feature: the client decrypts its local copy and writes Markdown files plus images.
No server endpoint is needed.

## Users

### `GET /users/lookup?username=alice` and `GET /users/{id}`
→ `200 {"user_id", "username", "public_key"}`. Only exact matches are returned; there is no user listing.
Clients pin the result (see [crypto.md](crypto.md#contact-keys-trust-on-first-use)).

## Sync

### `POST /sync`
Specified in [sync.md](sync.md). Request body limit: 16 MiB.

## Sharing

Only the owner can share, change permissions or revoke.

### `POST /memos/{id}/members`
```json
{ "user_id": "…", "permission": "read",
  "keys": [ { "key_version": 1, "wrapped_key": "…" }, { "key_version": 2, "wrapped_key": "…" } ] }
```
`keys` must contain every existing key version (`incomplete_keys` otherwise). → `201`. If the user is
already a member → `409`.

### `PATCH /memos/{id}/members/{user_id}`
```json
{ "permission": "write" }
```
→ `204`

### `POST /memos/{id}/revoke`
Removes members and rotates the key, as one atomic operation.
```json
{ "user_ids": ["…"],
  "base_version": 9, "new_key_version": 3, "ciphertext": "<payload as version 10 under key 3>",
  "keys": [ { "user_id": "<owner>", "wrapped_key": "…" }, { "user_id": "<remaining member>", "wrapped_key": "…" } ] }
```
`keys` must cover **exactly** the remaining members, owner included. → `200 {"version": 10}`. If
`base_version` is stale → `409`; the client syncs, re-encrypts and retries.

### `DELETE /memos/{id}/members/me`
Leave a memo shared with you. → `204`. The owner cannot leave (`403`); they delete the memo instead.

## Revisions

### `GET /memos/{id}/revisions`
→ `200 [{"version", "key_version", "updated_by", "updated_at", "size"}]`, newest first.

### `GET /memos/{id}/revisions/{version}`
→ `200 {"version", "key_version", "ciphertext"}`

Restoring an old revision means pushing its payload as a new version via `/sync`.

## Attachments

### `PUT /memos/{memo_id}/attachments/{id}/{kind}`
`kind` is `original` or `thumbnail`. The body is the envelope bytes, sent as
`Content-Type: application/octet-stream`. Requires `write` permission on the memo. The first `PUT`
for an `id` creates the attachment record (uploader = caller). The two kinds may arrive in either order.

| Response | When |
|---|---|
| `201` | Stored |
| `200` | Already stored with identical bytes (SHA-256 match), so a retry is safe |
| `409` | Already stored with different bytes |
| `413` | Over the size limit (default 25 MiB original, 1 MiB thumbnail) |
| `422 quota_exceeded` | Uploader's quota exceeded |

### `GET /attachments/{id}/{kind}`
Requires membership of the attachment's memo. Response headers:

```
Content-Type: application/octet-stream
Content-Disposition: attachment
X-Content-Type-Options: nosniff
Cache-Control: private, max-age=31536000, immutable
ETag: "<sha256 hex>"
```

Attachments are deleted with the `attachment.delete` sync op.

## Rate limits

| Endpoint | Limit |
|---|---|
| `/auth/prelogin` | 20/min per IP |
| `/auth/login` | 10/min per IP; per account, exponential backoff after 5 failures (no hard lockout, so nobody can lock someone else out) |
| `/auth/register` | 5/hour per IP |
| `/auth/recover*` | 5/hour per IP, 3/hour per account |
| everything else | 600/min per user |

The client IP is taken from `X-Forwarded-For` **only** if the connection comes from a configured
trusted proxy address.

## Later (not in v1)

- `GET /events`: SSE stream of `{"seq": n}` for instant sync.
- Public read-only share links.
- `Range` requests for large attachments.
