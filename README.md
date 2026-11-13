# memo

Self-hosted, end-to-end encrypted notes with offline sync and sharing between users.
Clients: web (Svelte), Linux desktop (Tauri), Android (Compose Multiplatform). Server: Rust (axum + SQLite).

**Status:** `crates/core`, `crates/server`, the web client, the Android app and the Linux desktop app are implemented. Start with [docs/README.md](docs/README.md).

```
crates/core     shared Rust: types, crypto, merge, sync engine
crates/server   memo-server
crates/sqlite   the native clients' local store
crates/wasm     core for the browser (WASM)
crates/native   core for the native apps: one JSON call, shared by Android and desktop
crates/android  that call over JNI
web/            Svelte frontend (browser + Tauri)
desktop/        Tauri app
android/        Android app
docs/           specification
scripts/        dev tools (maven-ages.py: no Android dependency younger than two weeks)
```

## Build

```sh
(cd web && bun install && bun run build)   # optional: the web app, embedded into the server
cargo build --release -p memo-server
```

See [web/README.md](web/README.md) for the web app's toolchain and development setup,
[android/README.md](android/README.md) for the Android app and [desktop/README.md](desktop/README.md)
for the desktop app.

## Release

Set the new version in `Cargo.toml` (`[workspace.package]`), `desktop/tauri.conf.json` and
`android/app/build.gradle.kts` (`versionName`, and raise `versionCode`), run `cargo check` so that
`Cargo.lock` follows, commit, and push a tag for that commit:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

The Release workflow (`.github/workflows/release.yml`) checks the tag against those versions and the
Rust dependencies against the RustSec advisory database, runs the tests, and publishes a GitHub
release: `memo-server` (with the web app) and `memo-desktop` for x86_64 and aarch64 Linux, the signed
Android APK, and `SHA256SUMS`. The notes are the tagged commit's message, or GitHub's generated notes
when it has no body. A tag with a hyphen, such as `v0.2.0-rc1`, becomes a pre-release. The workflow
needs the signing secrets in [android/README.md](android/README.md#release-build).
