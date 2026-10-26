# memo

Self-hosted, end-to-end encrypted notes with offline sync and sharing between users.
Clients: web (Svelte), Linux desktop (Tauri), Android (Compose Multiplatform). Server: Rust (axum + SQLite).

**Status:** `crates/core` (crypto, payloads, merge, API types, sync engine) is implemented; the server and clients are next. Start with [docs/README.md](docs/README.md).

```
crates/core     shared Rust: types, crypto, merge, sync engine
crates/server   memo-server
web/            Svelte frontend (browser + Tauri)
desktop/        Tauri app
android/        Android app
docs/           specification
```

## Build

```sh
cargo build
```
