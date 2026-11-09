# memo

Self-hosted, end-to-end encrypted, offline-first notes. MIT. Read [docs/README.md](docs/README.md)
first: `docs/` is the spec and the contract between server and clients. Change the spec first,
then the code, and keep them in sync.

## Status and what comes next

Done: `crates/core` (types, crypto, merge, sync engine), `crates/server` (axum + SQLite), `crates/wasm`
and the web client (`web/`, online-only, see `web/README.md`), and the Android app (`android/`,
offline-first, see `android/README.md`): Compose Multiplatform UI, `memo-core` over a hand-written
JNI bridge (`crates/android`) with the rusqlite `Store` (`crates/sqlite`). The user chose JNI over
UniFFI and rusqlite over SQLDelight to keep dependencies down.

Next: the **Linux desktop app** (`desktop/`, Tauri, offline-first). Its Rust side runs `memo-core`
with `crates/sqlite`, much like `crates/android` does. The frontend is the existing web UI with a
second implementation of the `Backend` interface (`web/src/lib/types.ts`) that calls Tauri commands
instead of the worker.

Known gaps are listed at the end of `web/README.md` and `android/README.md`.

## Dependencies

The user started this project to escape upstream churn. Dependencies are a liability:

- Prefer the standard library and mature, stable-API libraries with few transitive dependencies.
  Say so explicitly before adding anything pre-1.0 or fast-moving, and ask before adding a new one.
- Pin exact versions and update on purpose, never in passing.
- **Nothing published less than two weeks ago**, transitive dependencies included:
  - Rust: after every `Cargo.lock` change run `scripts/crate-ages.py`, and pin anything too new back
    with `cargo update -p <crate> --precise <version>`.
  - Web: `web/bunfig.toml` enforces it (`exact = true`, `minimumReleaseAge`). Install with Bun only.
  - Android/Gradle: every resolved artifact is pinned in the `gradle.lockfile`s. After changing a
    version, rewrite the locks (`android/README.md`) and run `scripts/maven-ages.py`.
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
(cd android && ./gradlew assembleDebug)                      # also builds libmemo.so
```

The Android app has no automated UI test yet: try changes on the emulator against a local server
(`android/README.md`).

The server embeds `web/dist` at compile time (`crates/server/build.rs`), so rebuild it after
rebuilding the web app. `web/e2e/run.ts` starts its own server on a temporary data directory.

## Rules that are easy to miss

- All cryptography, payload formats and merging live in `crates/core` only. Clients never
  reimplement them; the web app reaches them through WASM, Android through `crates/android`.
  Logic both bridges need (editing, resolving, read models) goes into core, not into a bridge.
- The web app runs under a strict CSP: no inline scripts or `style="…"` attributes, no Svelte
  transitions, no `data:` URLs (details in `web/README.md`).
- Match the surrounding code: naming, comment density, idiom. Docs are plain and short.
- Commit only when asked. Messages are short and imperative ("Add memo-server and the web client").
