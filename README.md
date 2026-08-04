# ClipSync

ClipSync is a cross-platform clipboard synchronization system organized as a single monorepo.

## Architecture

- `crates/core` (`clipboard-core`) owns shared Rust protocol data and will expose the complete client core through UniFFI FFI. Its default feature is intentionally data-only; verification and client dependencies are opt-in.
- `crates/server` (`clipboard-server`) is the Axum relay service. It enables only the core `verify` feature and does not link client crypto, QR, networking, or UniFFI dependencies.
- `tools/uniffi-bindgen` (`clipsync-uniffi-bindgen`) is the lockfile-pinned UniFFI binding generator wrapper.
- `macos` is the SwiftUI macOS 13 menu bar agent, identified by `com.clipsync.macos` and configured as an `LSUIElement` application.
- `android` is the Kotlin and Jetpack Compose application, identified by `com.clipsync.app`, with bundled ML Kit barcode scanning.
- `deploy`, `scripts`, and `dist` hold deployment assets, repository automation, and ignored build artifacts.

## Protocol Constants

| Constant | Value |
| --- | --- |
| Maximum WebSocket frame | 24 MiB |
| Maximum WebSocket message | 24 MiB |
| Encoded image cap | 10 MiB |
| Pairing code TTL | 300 seconds |
| Mailbox TTL | 7 days |
| Identities per room | 2 |
| Clipboard poll interval | 300 ms |
| Reconnect backoff | 1 second to 30 seconds, +/-20% jitter |
| History cap | 50 entries |
| Deduplication ring | 64 entries |
| Envelope version | v1 |
| HKDF info prefix | `clipboard-sync-v1` |
| UniFFI version | `=0.29.4` |

## Build Gates

```sh
cargo build --locked --workspace
(cd macos && swift build -Xswiftc -warnings-as-errors)
(cd android && ./gradlew assembleDebug)
```

Generate UniFFI bindings only through the workspace wrapper:

```sh
cargo run --locked -p clipsync-uniffi-bindgen -- <args>
```

## macOS Installation

1. Run `scripts/package-macos.sh`, then move `dist/ClipboardSync.app` to `/Applications`.
2. On first launch, Control-click the app and choose **Open** to approve the ad-hoc signed build.
3. Set the relay `wss://` URL, choose **Pair Device**, scan the QR code on Android, and confirm the same six-digit security code on both devices.

Open at Login can be enabled in ClipSync settings. If macOS requires approval, use the displayed button to open the Login Items settings page.

ClipSync is intentionally menu-bar-only (`LSUIElement`), so it does not add a Dock icon; pairing and history open as on-demand utility windows.

## Android Installation

1. Run `scripts/android-release.sh` to create `dist/clipboard-sync.apk`, then install that APK on the Android device.
2. Open ClipSync, scan the QR code shown by the Mac, and confirm the same six-digit security code on both devices. Confirmation is mandatory before pairing is saved.
3. Allow notifications and battery-optimization exemption so the foreground data-sync service can receive while the app is not open. On Xiaomi, Huawei, Honor, OPPO, OnePlus, realme, vivo, or iQOO devices, also enable ClipSync in the vendor's autostart settings shown in the app.

The first release build creates `android/keystore/release.jks` and `android/keystore/keystore.properties` with mode `0600`; both paths are ignored by Git. Back up both files together in a secure location. They are required to produce upgrades signed as the same Android application. Do not paste their contents into issue reports or build logs.
