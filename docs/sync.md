# Sync protocol

Goals: clients work fully offline; when they reconnect, everything converges; no edit is ever
silently lost; and the server never needs plaintext to do any of this.

## Model

- The server is the source of truth for **ordering**. It holds the current ciphertext of each entity
  and the version history of memos.
- Each client keeps a full local copy: decrypted memos, keys and state. It records what it changed
  locally as **dirty flags**, not as a queue of operations, so many edits to one memo collapse into
  a single push.
- All synced data moves through one endpoint, `POST /api/v1/sync`. Attachment bytes use separate
  binary endpoints. Sharing and account management are online-only REST calls (see [api.md](api.md)).

The sync engine (dirty tracking, building the push, applying results and changes, merging) is part of
`crates/core` (`memo_core::sync`). It does no I/O itself: it builds each request, the client sends it
with its own HTTP stack, and the engine applies the response. Storage sits behind the `Store` trait:

| Client | Store backend |
|---|---|
| Tauri desktop | SQLite on disk (offline-capable) |
| Android | SQLDelight store, reached through UniFFI callbacks (offline-capable) |
| Web | In-memory (online-only). Unsent edits are lost if the tab closes; the UI warns before leaving |

## Server: change log

```
change_log(seq, user_id, entity_type, entity_id)     -- one row per (user, entity)
```

Every write to an entity gets a new `seq`, from one global, strictly increasing counter. That `seq` is
recorded for **every user who can see the entity**:

| Entity | Fanned out to |
|---|---|
| `memo` | all members |
| `access` (keys and member list of a memo) | all members, plus a user who just lost access |
| `attachment` | all members |
| `state` (pinned/archived) | that user only |
| `vault` | that user only |

Writing an entity **replaces** its earlier change-log row for that user (delete + insert), so the log
holds just one row per entity a user can see. Sync returns the **current** state of each changed
entity, never the history.

Tombstones (deleted memos and attachments, revoked access) stay in the log for 90 days and are then
purged. For each user the server keeps `purged_through`, the highest `seq` it has removed for them.

## `POST /api/v1/sync`

Request:

```json
{
  "cursor": 1042,
  "push": [
    { "op": "memo.put", "id": "…", "base_version": 4, "key_version": 1, "ciphertext": "…" },
    { "op": "memo.put", "id": "…", "base_version": 0, "key_version": 1, "ciphertext": "…", "wrapped_key": "…" },
    { "op": "memo.delete", "id": "…", "base_version": 7 },
    { "op": "state.put", "memo_id": "…", "pinned": true, "archived": false },
    { "op": "attachment.delete", "id": "…" },
    { "op": "vault.put", "base_version": 3, "ciphertext": "…" }
  ]
}
```

Response:

```json
{
  "results": [
    { "status": "ok", "version": 5 },
    { "status": "ok", "version": 1 },
    { "status": "conflict", "current_version": 9 },
    { "status": "ok" },
    { "status": "ok" },
    { "status": "ok", "version": 4 }
  ],
  "changes": [ /* entities, see below, ordered by seq */ ],
  "cursor": 1057,
  "epoch": "3q2-7wE…",
  "more": false,
  "reset": false
}
```

The request may also carry the `epoch` the client last saw. The server answers `reset: true` if it
differs.

`created_at`, `updated_at` and `updated_by` are set by the server.

- `results[i]` belongs to `push[i]`. The server applies all ops in order within one transaction, but
  each op succeeds or fails on its own. Status values: `ok`, `conflict`, `deleted`, `forbidden`,
  `not_found`, `invalid`, `quota_exceeded`.
- `changes` is read **after** the push is applied, so a `conflict` is always followed by the newer
  version in the same response.
- `changes` is limited (500 entities or 4 MiB). If `more` is true, the client calls again with the new
  `cursor` and an empty `push`.
- When `cursor` is 0, the first page **starts with the `vault` entity**, wherever its `seq` falls, so a
  new device knows its contact pins before it verifies any wrapped key. The page's `cursor` ignores
  that out-of-order entry, and the vault may come again later at its own `seq`.
- A client's own successful writes also come back in `changes`. Clients skip entries whose version they
  already hold.
- `reset: true` is sent if `0 < cursor < purged_through`, if the request's `epoch` differs from the
  server's, or if `cursor` is greater than the server's current maximum `seq`. A reset response
  applies **nothing** from the push: `results` and `changes` are empty and `cursor` is 0. The `epoch` is a random
  value the server stores in its database. `memo-server restore` replaces it, so every client resyncs
  after a backup is restored (see [server.md](server.md#backups)). The `seq` check is a fallback for
  databases restored by hand. See [Reset](#reset).
- At most 500 ops per request.

### Server rules per op

**`memo.put`** with `base_version = 0` (create):
- The `id` must not exist. `key_version` must be 1 and `wrapped_key` present (`invalid` otherwise).
  The caller becomes the owner. The server stores the memo at version 1, plus `wrapped_key` as the
  owner's own access entry.
- If the id already exists and the caller is a member, answer `conflict`. This is how a create whose
  response was lost shows up; the client recognises its own write (see *In-flight writes*). An existing
  id the caller cannot see → `invalid`.

**`memo.put`** with `base_version > 0` (update):
- If the memo is deleted and the caller has its tombstone in their change log, answer `deleted`.
- The caller must be a member (`not_found` otherwise, the same answer as for a memo that never
  existed) and must have `write` permission (`forbidden`).
- There is no retry detection: nonces are random, so a resend never carries identical bytes. A resend
  whose first attempt landed gets `conflict`, and the client recognises its own write.
- If `base_version` ≠ the current version, or `key_version` ≠ the current key version, answer
  `conflict`. A key rotation always creates a new version, so the second case is a conflict too.
- Otherwise the server stores the new version (`base_version + 1`), records it in `memo_revisions`,
  sets `updated_by` and `updated_at`, and fans out the change.
- The ciphertext must not exceed 1 MiB (`invalid`), and must fit in the owner's quota (`quota_exceeded`).

**`memo.delete`**: owner only (`forbidden`). If `base_version` ≠ the current version, answer
`conflict`, so nobody deletes edits they have not seen. Deleting an already-deleted memo returns `ok`.
A delete drops the ciphertext and revisions, tombstones the memo and its attachments, removes all
access rows, and fans out to all former members.

**`state.put`**: the caller must be a member. Last write wins. It never conflicts.

**`attachment.delete`**: needs `write` permission on the attachment's memo. Idempotent. The blob is
purged 30 days later.

**`vault.put`**: `conflict` if `base_version` ≠ the current version. Otherwise the server stores
version `base_version + 1`.

## Change entities

```json
{ "type": "memo", "seq": 1050, "id": "…", "owner_id": "…", "version": 9, "key_version": 2,
  "ciphertext": "…", "deleted": false, "created_at": 0, "updated_at": 0, "updated_by": "…" }

{ "type": "access", "seq": 1051, "memo_id": "…", "owner_id": "…", "revoked": false, "permission": "write",
  "keys": [ { "key_version": 1, "wrapped_key": "…" }, { "key_version": 2, "wrapped_key": "…" } ],
  "members": [ { "user_id": "…", "username": "alice", "public_key": "…", "permission": "write" } ] }

{ "type": "access", "seq": 1052, "memo_id": "…", "revoked": true }

{ "type": "attachment", "seq": 1053, "id": "…", "memo_id": "…", "size": 2412371,
  "thumb_size": 31002, "uploaded": true, "thumb_uploaded": true, "deleted": false }

{ "type": "state", "seq": 1054, "memo_id": "…", "pinned": true, "archived": false }

{ "type": "vault", "seq": 1055, "version": 4, "ciphertext": "…" }
```

A deleted memo carries no ciphertext. In `access`, `permission` is the caller's own, `keys` only
contains keys wrapped **for the caller**, and `members` lists everyone with access, the owner included,
with the public key the server holds for them. Clients compare those keys with their pins
(see *Applying an access change*); they never use one that differs.

## Client: local state

Per memo:

| Field | Meaning |
|---|---|
| `server_version`, `key_version` | Last version received from or accepted by the server |
| `base` | Decrypted payload at `server_version`: the merge base |
| `local` | Edited payload, or empty if the memo is clean. A non-empty `local` means the memo is dirty |
| `highest_seen_version` | Rollback protection |
| `pending_delete` | The user deleted it while offline |
| `conflict` | Empty, or `{theirs, theirs_version}` while it waits for the user to resolve it |
| `in_flight` | The payloads of `memo.put`s sent for the next version whose result never arrived |
| `undecoded` | A newer server version that cannot be used yet: no key, decryption failed, or newer format |
| `push_error` | `forbidden`, `invalid` or `quota_exceeded` from the last push (see below) |

There are also dirty flags for `state` per memo and for the `vault`, plus per attachment
`pending_upload` (original/thumbnail) and `pending_delete`. A new memo created offline is simply a
dirty memo with `server_version = 0`.

## Client: sync round

1. **Build the push** from dirty state, in this order: the vault, memos, states, attachment deletes. Each
   dirty memo becomes one `memo.put`, built from `local`, with `base_version = server_version` and
   encrypted for version `server_version + 1`. The client records the snapshot in `in_flight` (persisted
   before sending). Memos with a `conflict`, an `undecoded` version or a `push_error` are not pushed;
   `quota_exceeded` is retried once per sync trigger.
2. **Send `POST /sync`**, then keep calling while `more` is true.
3. **Apply results:**
   - `ok`: set `server_version` to the returned version and `base` to the pushed snapshot. If `local`
     still equals that snapshot, clear it. Otherwise the user kept typing and the memo stays dirty.
     Attachments the write removed (in the old `base`, in neither the new `base` nor `local`) get an
     `attachment.delete`.
   - `conflict`: do nothing yet. The newer version is in `changes`, and step 4 handles it.
   - `deleted` / `not_found`: see *Special cases*.
   - `forbidden`: the user lost write permission. Keep `local`, show the memo read-only, and offer
     to save the edit as a private copy. Cleared if write permission comes back.
   - `invalid`: log it, keep the data, and surface "could not sync this memo". Cleared by the next edit.
4. **Apply changes** in `seq` order (memo handling is below).
5. **Upload pending attachment blobs** for memos that now exist on the server. Uploads are idempotent
   `PUT`s, retried with backoff.
6. If step 4 made memos dirty again (merges, new contact pins) or the push was cut at 500 ops, run
   another round. At most 8 rounds per trigger.
7. Store the new `cursor`, in the same local transaction as the applied changes (one transaction per
   page).

Within a page, the client applies the `vault` entity first.

### In-flight writes

If a response is lost, the client cannot know whether its `memo.put` landed. It does not need to: the
next push for that memo goes out with the same `base_version`, so at most one attempt can have landed,
and that one comes back as a change with `version = in_flight.version`. If its payload equals any
snapshot in `in_flight`, it is the client's own write: it becomes `base`, and `local` stays dirty only
if the user kept editing. Without this, typing after a lost response would conflict with itself.

### Applying a memo change

1. If `version < highest_seen_version`, ignore it and record a rollback warning (except during a reset).
   If the version is not newer than the one held, skip it (the client's own writes come back like this).
2. Decrypt the payload with the verified `memo_key[key_version]` (see *Applying an access change*). If
   the key is not there yet, keep the ciphertext in `undecoded` and retry when the access change
   arrives. If decryption fails, mark the memo "cannot decrypt" and keep the last good local copy.
   The client also retries `undecoded` memos at the start of each sync, so an app update can read a
   newer payload format.
3. If it matches `in_flight`, it is the client's own write (see *In-flight writes*).
4. If the memo is **clean**: set `base` to the new payload and `server_version` to the new version. A
   pending local delete is cancelled (see *Special cases*).
5. If the memo is **dirty**: run a three-way merge of `base` (old), `local` (ours) and the new payload (theirs).
   - **Clean merge:** `local` becomes the merged result, `base` and `server_version` take the new
     version, and the memo stays dirty so the next round pushes the merge. If the merged result equals
     theirs, the memo is clean.
   - **Overlapping edits:** set `conflict = {theirs, theirs_version}` and keep `local` unchanged. The
     UI shows both versions side by side. When the user resolves, the result becomes `local`, `base`
     and `server_version` become theirs, and the conflict is cleared. Newer versions that arrive while
     the conflict is open replace `theirs`.

### Applying an access change

1. `revoked: true`: handle as *Access revoked* below.
2. For every entry in `members` (other than the user), compare `public_key` with the pin in the vault.
   No pin: pin it now (trust on first use; the vault becomes dirty). A different pin: keep using the
   pinned key and raise the key-change alarm. For the user's own entry, the key must equal their own.
3. Open each `wrapped_key` not already held with HPKE auth mode, using the **pinned** key of
   `owner_id`. A key that fails is not stored, and the user sees a warning naming the owner.
4. Store the permission, members and keys, then retry the memo if it was waiting for a key.

### Merge rules

- `body`: line-based diff3 (via `diffy`). If changes overlap, it's a conflict and nothing is merged
  automatically.
- `attachments`: three-way set merge by `id`. An attachment is kept if either side has it and neither
  side removed it compared to `base`. If both sides changed the same entry's metadata, theirs wins.
- `edited_at`: the larger of the two.
- Vault `contacts`: union. If the two sides pin different keys for the same user, that's the
  key-change alarm from [crypto.md](crypto.md#contact-keys-trust-on-first-use); the older pin is kept.
  `verified` is true if either side verified the **same** key.
- Vault `settings`: per key, like `body` but without conflicts: theirs wins if both sides changed a key.

## Special cases

| Situation | Client behaviour |
|---|---|
| Local edit, but the memo was **deleted** on the server (`deleted`) | Save the edit as a **new private memo**, a "restored copy" with a new id and key. Re-upload the attachments it has cached and drop references to the rest. Tell the user |
| Local edit, but **access was revoked** (`not_found`, or an `access` entity with `revoked: true`) | Same as above: a restored private copy. Then delete the local copy of the memo and its cached blobs |
| Local delete conflicts with a newer version | Keep the memo, clear `pending_delete`, and tell the user it changed on another device or was edited by a member |
| Payload `v` higher than supported | Show the memo read-only and never push it. Keep the ciphertext so a newer client version can read it |
| Access gained (new `access` entity) | Pin the owner's key if needed (TOFU), verify the keys, then decrypt the memo, which arrives in the same or a later change |

## Reset

When the response has `reset: true`, the client runs a full resync:

1. `cursor = 0`, push nothing. Pull all pages. The server returns the current state of every entity the
   user can see. Rollback protection is off for these pages. If the app stops halfway, the next sync
   starts the reset over.
2. **The server lost data** (restored from a backup, or a rollback attack). If the local
   `server_version` of a memo is greater than the server's version, the client keeps its local content.
   It marks the memo dirty against the server's version, with a merge base equal to the server's
   payload, so the newer content goes back up. In effect, devices restore what the backup lost. The
   client shows a notice: "the server returned older data than this device had". If the server has the
   **same** version with different content (another device rewrote it after the restore), the memo
   goes into `conflict`.
3. Local entities that are not in the full set:
   - Memos never pushed (`server_version = 0`) and attachments not uploaded yet stay.
   - If the reset came with a **new epoch** (`memo-server restore`), the server lost them. The user's
     own memos are created again with the same id and `memo_key[1]`, as a `memo.put` with
     `base_version = 0`, and their cached blobs are uploaded again. Sharing must be redone. Memos owned
     by others are handled like deleted ones.
   - Otherwise (same epoch: offline for longer than the tombstone retention) they were deleted. Clean
     ones are removed; dirty ones are handled as in *Special cases*.

## Attachments transfer

- A new attachment: the client stores the encrypted original and thumbnail locally, adds the reference
  to the memo payload, and marks the blobs `pending_upload`.
- Uploads (`PUT …/attachments/{id}/{original|thumbnail}`) happen after the memo exists on the server
  (step 5). A completed upload fans out an `attachment` change with `uploaded: true`.
- Other devices might receive the memo before the blobs. They show a placeholder and download once
  `uploaded` is true.
- Downloads are lazy: thumbnails when a memo is displayed (native clients may prefetch them on
  unmetered networks), originals when the image is opened. Native clients keep an LRU cache of
  the **encrypted** blobs (default 1 GiB) and decrypt them in memory on display.

## When sync runs

| Trigger | Desktop (Tauri) | Android | Web |
|---|---|---|---|
| App start / login | ✓ | ✓ | ✓ |
| Window focus / app foreground | ✓ | ✓ | ✓ |
| Network regained | `online` event + retry with backoff | WorkManager job, `NetworkType.CONNECTED` | `online` event |
| 2 s after the last local edit | ✓ | ✓ | ✓ |
| Periodic while in foreground | every 5 min | every 5 min | every 5 min |
| Background | — | WorkManager periodic, 1 h | — |

Later: `GET /api/v1/events` (SSE) sends `{"seq": n}` whenever the user's log advances, so foreground
clients sync right away. The protocol does not change for this.
