# memo

Self-hosted, end-to-end encrypted notes with offline sync and sharing between users.
Clients: web (Svelte), Linux desktop (Tauri), Android (Compose Multiplatform). Server: Rust (axum + SQLite).

**Status:** `crates/core`, `crates/server` and the web client are implemented; the desktop and Android clients are next. Start with [docs/README.md](docs/README.md).

```
crates/core     shared Rust: types, crypto, merge, sync engine
crates/server   memo-server
crates/wasm     core for the browser (WASM)
web/            Svelte frontend (browser + Tauri)
desktop/        Tauri app
android/        Android app
docs/           specification
```

## Build

```sh
(cd web && bun install && bun run build)   # optional: the web app, embedded into the server
cargo build --release -p memo-server
```

See [web/README.md](web/README.md) for the web app's toolchain and development setup.
