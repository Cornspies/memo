# memo — specification

memo is a self-hosted, end-to-end encrypted note-taking app. Notes ("memos") are Markdown with image
attachments. They sync across devices, work offline, and can be shared with specific users on the
same server.

These documents are the contract between the server and every client. Change the spec first, then the code.

| Document | Contents |
|---|---|
| [threat-model.md](threat-model.md) | What the encryption protects against, and what it does not |
| [crypto.md](crypto.md) | Key hierarchy, primitives, encodings, account and sharing flows |
| [sync.md](sync.md) | Offline-first sync protocol, conflict handling, client state machine |
| [api.md](api.md) | HTTP API reference (`/api/v1`) |
| [server.md](server.md) | Database schema, storage layout, limits, configuration, admin CLI |

## Components

```
crates/core     Rust: types, crypto, merge, sync engine. Used by every component.
crates/server   Rust (axum + SQLite): API, auth, encrypted blob storage. Never decrypts.
web/            Svelte + TypeScript. Browser (online-only) and Tauri frontend. Uses core via WASM.
desktop/        Tauri. Its Rust side runs core natively with a local SQLite store (offline).
android/        Compose Multiplatform. Uses core via UniFFI; SQLDelight store; WorkManager sync.
```

All cryptography, the payload formats and the three-way merge live in `crates/core`, and only there.
Clients never reimplement them.

## Principles

- **Few dependencies, stable ones.** Every dependency is a liability for an app meant to run for years.
  Prefer the standard library and mature crates. Pin versions and update on purpose.
- **The server is untrusted for confidentiality.** It stores ciphertext and enforces access control,
  quotas and rate limits. It never sees plaintext content, keys or passwords.
- **Nothing is silently lost.** Conflicting edits are merged or kept for the user to resolve.
- **The API is versioned.** `/api/v1` stays backwards compatible. Breaking changes get `/api/v2`, and the
  server serves both until old clients are gone.
- **Encodings are versioned.** Every ciphertext envelope and plaintext payload carries a format
  version, so formats can evolve without breaking stored data.

## Glossary

| Term | Meaning |
|---|---|
| memo | A note: an encrypted Markdown body plus attachment references |
| owner | The user who created a memo. Only the owner shares, revokes or deletes it |
| member | A user with access to a memo (the owner included), with permission `read` or `write` |
| memo key | Random symmetric key for a memo. Rotated when a member is removed (`key_version`) |
| account key | Random symmetric key per user that protects the user's private key and vault |
| vault | Small encrypted per-user document: pinned contact keys and settings |
| device | One logged-in client instance with its own revocable token |
| cursor | Position in the per-user change log, used by sync |
