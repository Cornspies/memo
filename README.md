# memo

Self-hosted, end-to-end encrypted notes with offline sync and sharing between users.
Clients: web (Svelte), Linux desktop (Tauri), Android (Compose Multiplatform). Server: Rust (axum + SQLite).

**Status:** `crates/core`, `crates/server`, the web client and the Android app are implemented; the desktop client is next. Start with [docs/README.md](docs/README.md).

```
crates/core     shared Rust: types, crypto, merge, sync engine
crates/server   memo-server
crates/sqlite   the native clients' local store
crates/wasm     core for the browser (WASM)
crates/android  core for the Android app (JNI)
web/            Svelte frontend (browser + Tauri)
desktop/        Tauri app
android/        Android app
docs/           specification
scripts/        dev tools (crate-ages.py, maven-ages.py: no dependency younger than two weeks)
```

## Build

```sh
(cd web && bun install && bun run build)   # optional: the web app, embedded into the server
cargo build --release -p memo-server
```

See [web/README.md](web/README.md) for the web app's toolchain and development setup, and
[android/README.md](android/README.md) for the Android app.
