# memo

Self-hosted, end-to-end encrypted, offline-first notes. MIT. Read [docs/README.md](docs/README.md)
first: `docs/` is the spec and the contract between server and clients. Change the spec first,
then the code, and keep them in sync.

## Status and what comes next

Done: `crates/core` (types, crypto, merge, sync engine), `crates/server` (axum + SQLite), `crates/wasm`
and the web client (`web/`, online-only, see `web/README.md`).

Next, in this order:

1. **Linux desktop app** (`desktop/`, Tauri, offline-first). Its Rust side runs `memo-core` natively
   with a SQLite implementation of the `Store` trait (the web uses `MemoryStore`). The frontend is
   the existing web UI with a second implementation of the `Backend` interface
   (`web/src/lib/types.ts`) that calls Tauri commands instead of the worker.
2. **Android app** (`android/`): Compose Multiplatform, core via UniFFI, SQLDelight store,
   WorkManager sync.

Known gaps in the web app are listed at the end of `web/README.md`.

## Dependencies

The user started this project to escape upstream churn. Dependencies are a liability:

- Prefer the standard library and mature, stable-API libraries with few transitive dependencies.
  Say so explicitly before adding anything pre-1.0 or fast-moving, and ask before adding a new one.
- Pin exact versions and update on purpose, never in passing.
- **Nothing published less than two weeks ago**, transitive dependencies included:
  - Rust: after every `Cargo.lock` change run `scripts/crate-ages.py`, and pin anything too new back
    with `cargo update -p <crate> --precise <version>`.
  - Web: `web/bunfig.toml` enforces it (`exact = true`, `minimumReleaseAge`). Install with Bun only.
  - Android/Gradle (later): there is no such setting. Check the publish dates by hand.
- `wasm-bindgen` in `crates/wasm/Cargo.toml` is pinned with `=` and must match the installed
  `wasm-bindgen-cli`. Bump both together.

## Checks

Run these before saying something works:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p memo-wasm --target wasm32-unknown-unknown -- -D warnings
cargo fmt --check
(cd web && bun run check)                                    # svelte-check
(cd web && bun run build) && cargo build --release -p memo-server
(cd web && bun run e2e)                                      # headless Firefox, needs the build above
```

The server embeds `web/dist` at compile time (`crates/server/build.rs`), so rebuild it after
rebuilding the web app. `web/e2e/run.ts` starts its own server on a temporary data directory.

## Rules that are easy to miss

- All cryptography, payload formats and merging live in `crates/core` only. Clients never
  reimplement them; the web app reaches them through WASM.
- The web app runs under a strict CSP: no inline scripts or `style="…"` attributes, no Svelte
  transitions, no `data:` URLs (details in `web/README.md`).
- Match the surrounding code: naming, comment density, idiom. Docs are plain and short.
- Commit only when asked. Messages are short and imperative ("Add memo-server and the web client").
