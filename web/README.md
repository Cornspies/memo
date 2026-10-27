# memo web app

The browser client: Svelte 5 + TypeScript, built with Vite and embedded into `memo-server`. It is
online-only (`docs/sync.md`): memos live in memory and sync right away. The UI warns before the tab
closes while changes are unsynced.

All crypto, sync and merging run in `memo-core`, compiled to WASM (`crates/wasm`) and run in a
worker, so Argon2 and decryption never block the page. Keys exist only in that worker's memory.
After a reload the session cookie is still valid, but the keys are gone: the password unlocks again,
and that login replaces the device instead of adding one.

## Toolchain

- Rust with the `wasm32-unknown-unknown` target: `rustup target add wasm32-unknown-unknown`
- The `wasm-bindgen` CLI at **exactly** the version pinned in `crates/wasm/Cargo.toml`:
  `cargo install wasm-bindgen-cli --version 0.2.129 --locked`. Bump both together.
- [Bun](https://bun.sh) as the package manager. `bunfig.toml` installs exact versions only, and
  nothing published in the last two weeks.

## Commands

```sh
bun install
bun run build    # WASM (build-wasm.sh) + Vite → dist/, which `cargo build -p memo-server` embeds
bun run check    # svelte-check (types and Svelte diagnostics)
bun run wasm     # only rebuild src/wasm/ after changing Rust code
bun run dev      # Vite on http://localhost:5173, proxying /api to 127.0.0.1:8080
bun run e2e      # end-to-end test in headless Firefox (see below)
```

For `bun run dev`, run a local `memo-server` with `public_url = "http://localhost:5173"`. The
server only accepts cookie-authenticated writes whose `Origin` matches `public_url`.

## End-to-end test

`e2e/run.ts` drives the app in headless Firefox over WebDriver BiDi (`e2e/bidi.ts`, no extra
dependencies). It starts `../target/release/memo-server` on a temporary data directory and walks through
registration, editing, images, unlock after reload, sharing, a conflict, recovery, history, revoking,
archive, export, password change and the phone layout. It needs `firefox` on the PATH and a server built
after `bun run build`:

```sh
bun run build && cargo build --release -p memo-server && bun run e2e
```

`MEMO_SERVER` overrides the server binary, `E2E_SHOTS=<dir>` saves screenshots, and `E2E_KEEP=1`
keeps the temporary directory. A failure prints the step, the browser console and the server log.

## Layout

```
src/lib/types.ts        Backend interface and the read models (mirrors memo_core::sync::view)
src/lib/backend.ts      Backend for the browser: forwards calls to the worker
src/lib/worker.ts       the worker: WASM client, HTTP, sync scheduling, account flows
src/lib/images.ts       image preparation (type check, metadata, rotation, thumbnails)
src/lib/state.svelte.ts session, sync status, notifications, routing
src/components/         the UI
e2e/                    the end-to-end test
```

The desktop app will reuse the UI with its own `Backend` (Tauri commands, SQLite store).

## Content Security Policy

The server sends `script-src 'self' 'wasm-unsafe-eval'; style-src 'self'; img-src 'self' blob:`.
So: no inline scripts or `style="…"` attributes (use classes, or Svelte's `style:` directive, which
goes through the CSSOM), no Svelte transitions (they inject `<style>` elements), no `data:` URLs.
Rendered memo HTML comes from `memo_core::render`, which escapes raw HTML.

## Not done yet

- "Keep unlocked in this browser" (`docs/crypto.md#account-flows`): the keys are always in memory only.
- Downscaling images on upload is supported by `images.ts` but has no setting in the UI yet.
