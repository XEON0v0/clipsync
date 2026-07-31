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
