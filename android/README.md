# memo for Android

Offline-first: every memo, key and image you open lives on the device, and sync catches up when the
server is reachable (`docs/sync.md`). The UI is Compose Multiplatform; everything else runs in
`memo-core`, compiled for Android (`crates/android` over `crates/native`), with its SQLite store
(`crates/sqlite`).

- **Kotlin ↔ Rust:** one JNI function, `Native.call(request, data)`: a JSON request as UTF-8 bytes,
  optional bytes (images, blobs), and an encoded reply (see `crates/native/src/lib.rs`).
- **Keys:** after sign-in, the device token, account key and private key are kept in a file
  encrypted with an Android Keystore key (`AccountFile.kt`). The app opens without the password.
  Signing out, or the server revoking the device, deletes the local copy.
- **HTTP:** `HttpURLConnection` with the device token (`Http.kt`).
- **Sync:** on start, on returning to the app, 2 s after an edit, every 5 min in the foreground,
  when the network comes back, and hourly in the background (WorkManager).
- **Memo bodies** are drawn natively from `memo_core::render::to_blocks`, never in a web view.
- **Images:** memo-core checks and strips them; Android applies the EXIF rotation, downscales to
  2560 px (a setting, on by default), converts HEIC to JPEG and makes WebP thumbnails (`Images.kt`).

## Toolchain

- JDK 17 or newer (Android Studio's bundled one works), Android SDK with platform 37
  (`ANDROID_HOME`, default `~/Android/Sdk`).
- NDK **27.3.13750724** (r27d), the version in `build-rust.sh` and `app/build.gradle.kts`.
- Rust targets: `rustup target add aarch64-linux-android x86_64-linux-android`.

Gradle comes from the wrapper (9.8.2, checksum-pinned). Each build runs `build-rust.sh`, which
builds `libmemo.so` for arm64 phones and the x86_64 emulator, 16 KB page aligned.

```sh
./gradlew assembleDebug                 # app/build/outputs/apk/debug/app-debug.apk
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Debug builds may use plain HTTP, so the emulator can reach a local `memo-server` at
`http://10.0.2.2:8080`. Release builds only talk HTTPS.

## Dependencies

Versions are exact in `gradle/libs.versions.toml`, and every resolved artifact, transitive ones
included, is pinned in the `gradle.lockfile`s. After changing a version:

```sh
./gradlew buildEnvironment dependencies :shared:dependencies :app:dependencies --write-locks
../scripts/maven-ages.py                # nothing published less than two weeks ago
```

## Layout

```
shared/   the UI (Compose Multiplatform): screens, the `Backend` and `Platform` interfaces
app/      Android: AndroidBackend (memo-core, HTTP, sync), keystore, images, WorkManager, activity
build-rust.sh   libmemo.so into app/src/main/jniLibs (not checked in)
```

## Not done yet

- No automated UI test; the flows were checked by hand on the emulator against a local server and
  the web client (sign-up, offline edits, images, conflicts, sharing, revoking, history, recovery).
- Release signing is not set up.
- Attachments that cannot be shown inline are not offered for download.
