# memo for Linux

Offline-first, like the Android app: every memo, key and image you open is kept on the computer,
and sync catches up when the server is reachable (`docs/sync.md`). The window is Tauri (WebKitGTK)
showing the web UI (`web/`); everything else runs on the Rust side: `memo-core` with its SQLite store
(`crates/native`, `crates/sqlite`), the HTTP, and the keyring.

- **Frontend ↔ Rust:** one Tauri command, `call`, with the call's name in the `memo-call` header and
  raw bytes both ways (`src/calls.rs`). The frontend's `NativeBackend` (`web/src/lib/native.ts`)
  implements the same `Backend` as the browser's worker, so the UI is shared.
- **Keys:** after sign-in, the device token, account key and private key are stored in the Secret
  Service (GNOME Keyring, KWallet) under service `memo`, user `account`. The app opens without the
  password. Without a Secret Service the keys stay in memory, and every start asks for the password
  and needs the server. The token and keys stay on the Rust side: the web view never gets them, so
  a script that got into it could not carry them off.
- **Lock** removes the keys and signs the device out; the memos stay, and unlocking signs in again.
  **Sign out**, or the server revoking the device, deletes the local copy (`docs/crypto.md`).
- **HTTP:** `ureq` with rustls and the system's certificates. The webview never talks to the server.
  The Rust side adds the token, and once signed in it only talks to the account's server.
- **Sync:** on start, on focus, 2 s after an edit, every 5 min, and when the network comes back.
- **Links** in memos open in the default browser (`xdg-open`, http/https/mailto only). Exports are
  saved to the Downloads folder.

Data lives in `~/.local/share/memo.desktop/` (mode `0700`): `account.json` (server, username; no
secrets) and `db/<user id>.db`.

## Build

Needs WebKitGTK 4.1 and its development files (Arch: `webkit2gtk-4.1`; Debian/Ubuntu:
`libwebkit2gtk-4.1-dev`), and the web toolchain (`web/README.md`). The web app is embedded at
compile time, so build it first:

```sh
(cd web && bun run build)
cargo build --release -p memo-desktop   # target/release/memo-desktop
```

## Test

The debug build has a test mode: `memo-desktop --serve <port> <data dir> <web dist>` serves the web
UI and answers the same calls over HTTP, with secrets in a file instead of the keyring. The
end-to-end test drives it in headless Firefox against a real `memo-server`: registration, an image,
an offline edit, a restart, sync, the web client seeing the edit and back, lock and unlock, and
sign-out.

```sh
(cd web && bun run build) && cargo build -p memo-desktop && cargo build --release -p memo-server
(cd web && bun run e2e:desktop)
```

The Tauri window itself and the real keyring are not covered: try those by hand.

## Not done yet

- No packaging (`.desktop` file, icons, AppImage or distribution packages).
- Locking while offline cannot sign the device out on the server; remove it from the device list.
- Without a Secret Service each start is a new login, so the device list grows.
- Thumbnails are not prefetched; they download when a memo is shown.
