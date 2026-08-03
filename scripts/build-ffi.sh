#!/usr/bin/env bash
# T10: pinned UniFFI 0.29.4 FFI build + verified hosts.
#
# Contract:
# - every core build for FFI hosts uses exactly
#   `--no-default-features --features full`; the server builds with `verify`
#   only. The feature set is printed and asserted BEFORE any artifact builds.
# - binding generation goes ONLY through the workspace wrapper
#   (`cargo run --locked -p clipsync-uniffi-bindgen -- ...`), exact version
#   `uniffi-bindgen 0.29.4`; the tree must contain only `uniffi v0.29.4`.
# - Apple: XCFramework from the two pinned Darwin targets. Android: three ABIs
#   via pinned cargo-ndk 3.5.4 + NDK 27.2.12479018. Missing Android tooling is
#   reported as UNVERIFIED, never silently skipped.
# - tools are located via `command -v` (T18 review: no hardcoded rustup path).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_HTTP_PROXY=""
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://mirrors.ustc.edu.cn/rust-static}"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy || true

OUT_DIR="$ROOT_DIR/dist/ffi"
GEN_DIR="$OUT_DIR/generated"
ANDROID_GEN_DIR="$OUT_DIR/generated-android"
HEADERS_DIR="$OUT_DIR/apple-headers"
HOST_DIR="$OUT_DIR/host"
DEBUG_LIB_DIR="$ROOT_DIR/target/debug"
RELAY_PID=""
ANDROID_UNVERIFIED=""

CORE_FEATURES="--no-default-features --features full"

step() { printf '\n== %s ==\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

cleanup() {
    if [[ -n "$RELAY_PID" ]] && kill -0 "$RELAY_PID" >/dev/null 2>&1; then
        kill "$RELAY_PID" >/dev/null 2>&1 || true
        wait "$RELAY_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

# macOS has no `timeout`; run a host binary under a wall-clock watchdog.
run_with_timeout() {
    local seconds="$1"; shift
    "$@" &
    local pid=$!
    local deadline=$((SECONDS + seconds))
    while kill -0 "$pid" 2>/dev/null; do
        if (( SECONDS >= deadline )); then
            kill -9 "$pid" 2>/dev/null || true
            wait "$pid" 2>/dev/null || true
            fail "host program exceeded the ${seconds}s watchdog: $*"
        fi
        sleep 0.2
    done
    wait "$pid"
}

step "preflight: tool locations via command -v"
CARGO_BIN="$(command -v cargo)" || fail "cargo not found on PATH"
RUSTUP_BIN="$(command -v rustup)" || fail "rustup not found on PATH"
SWIFTC_BIN="$(command -v swiftc)" || fail "swiftc not found on PATH"
XCODEBUILD_BIN="$(command -v xcodebuild)" || fail "xcodebuild not found on PATH"
printf 'cargo=%s\nrustup=%s\nswiftc=%s\nxcodebuild=%s\n' \
    "$CARGO_BIN" "$RUSTUP_BIN" "$SWIFTC_BIN" "$XCODEBUILD_BIN"

step "preflight: pinned Rust targets"
for target in \
    aarch64-apple-darwin \
    x86_64-apple-darwin \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android; do
    if ! "$RUSTUP_BIN" target list --installed | grep -qx "$target"; then
        "$RUSTUP_BIN" target add "$target"
    fi
    "$RUSTUP_BIN" target list --installed | grep -qx "$target" \
        || fail "rustup target unavailable: $target"
    printf 'PASS: rustup target available: %s\n' "$target"
done

step "preflight: Android native toolchain (pinned cargo-ndk 3.5.4 + NDK 27.2.12479018)"
NDK_DIR=""
if cargo ndk --version 2>/dev/null | grep -q "3\.5\.4"; then
    printf 'cargo-ndk: %s\n' "$(cargo ndk --version 2>/dev/null | head -1)"
    for candidate in \
        "${ANDROID_NDK_HOME:-}" \
        "${ANDROID_HOME:-}/ndk/27.2.12479018" \
        "$HOME/Library/Android/sdk/ndk/27.2.12479018"; do
        if [[ -n "$candidate" && -f "$candidate/source.properties" ]] \
            && grep -q "Pkg.Revision = 27.2.12479018" "$candidate/source.properties"; then
            NDK_DIR="$candidate"
            break
        fi
    done
fi
if [[ -z "$NDK_DIR" ]]; then
    ANDROID_UNVERIFIED="cargo-ndk 3.5.4 and/or NDK 27.2.12479018 not installed"
    printf 'UNVERIFIED: Android ABI builds skipped — %s\n' "$ANDROID_UNVERIFIED" >&2
else
    printf 'PASS: pinned NDK at %s\n' "$NDK_DIR"
fi

step "feature contract: print and assert before building artifacts"
printf 'core FFI builds use: cargo build --locked -p clipboard-core %s\n' "$CORE_FEATURES"
printf 'server builds use:   cargo build --locked -p clipboard-server (core verify only)\n'
SERVER_TREE="$(cargo tree --locked -p clipboard-server --edges normal)"
for banned in uniffi chacha20poly1305 x25519-dalek hkdf qrcode "image v"; do
    if grep -q "$banned" <<<"$SERVER_TREE"; then
        fail "server production graph gained forbidden dependency: $banned"
    fi
done
printf 'PASS: server graph has no uniffi / crypto-full deps (verify feature only)\n'

BINDGEN_VERSION="$(cargo run --locked -p clipsync-uniffi-bindgen -- --version)"
printf 'bindgen --version: %s\n' "$BINDGEN_VERSION"
[[ "$BINDGEN_VERSION" == "uniffi-bindgen 0.29.4" ]] \
    || fail "expected exact 'uniffi-bindgen 0.29.4'"
UNIFFI_VERSIONS="$(cargo tree --locked -p clipboard-core $CORE_FEATURES --prefix none \
    | grep -oE 'uniffi v[0-9]+\.[0-9]+\.[0-9]+' | sort -u)"
printf 'core uniffi versions in tree:\n%s\n' "$UNIFFI_VERSIONS"
[[ "$UNIFFI_VERSIONS" == "uniffi v0.29.4" ]] \
    || fail "core tree must contain only uniffi v0.29.4"
printf 'PASS: pinned uniffi 0.29.4 verified (bindgen + tree)\n'

step "core dylib (debug; required for ws:// loopback host verification)"
printf '+ cargo build --locked -p clipboard-core %s\n' "$CORE_FEATURES"
cargo build --locked -p clipboard-core $CORE_FEATURES
[[ -f "$DEBUG_LIB_DIR/libclipboard_core.dylib" ]] || fail "core dylib missing"
# rustc stamps the dylib id with the intermediate deps/ path, where a stale
# cargo-check artifact can shadow the real library at runtime; pin the id to
# the canonical path hosts link against.
install_name_tool -id "$DEBUG_LIB_DIR/libclipboard_core.dylib" \
    "$DEBUG_LIB_DIR/libclipboard_core.dylib"

step "generate Swift + Kotlin bindings through the workspace wrapper"
rm -rf "$GEN_DIR" "$ANDROID_GEN_DIR"
mkdir -p "$GEN_DIR" "$ANDROID_GEN_DIR"
printf '+ cargo run --locked -p clipsync-uniffi-bindgen -- generate ...\n'
cargo run --locked -p clipsync-uniffi-bindgen -- generate \
    --library "$DEBUG_LIB_DIR/libclipboard_core.dylib" \
    --config "$ROOT_DIR/crates/core/uniffi-jvm.toml" \
    --language swift \
    --language kotlin \
    --out-dir "$GEN_DIR"
[[ -f "$GEN_DIR/clipboard_core.swift" \
    && -f "$GEN_DIR/clipboard_coreFFI.h" \
    && -f "$GEN_DIR/clipboard_coreFFI.modulemap" \
    && -f "$GEN_DIR/uniffi/clipboard_core/clipboard_core.kt" ]] \
    || fail "binding artifacts incomplete"
printf '+ cargo run --locked -p clipsync-uniffi-bindgen -- generate --config crates/core/uniffi.toml --language kotlin ...\n'
cargo run --locked -p clipsync-uniffi-bindgen -- generate \
    --library "$DEBUG_LIB_DIR/libclipboard_core.dylib" \
    --config "$ROOT_DIR/crates/core/uniffi.toml" \
    --language kotlin \
    --out-dir "$ANDROID_GEN_DIR"
[[ -f "$ANDROID_GEN_DIR/uniffi/clipboard_core/clipboard_core.kt" ]] \
    || fail "Android Kotlin binding artifact missing"
# UniFFI 0.29.4 emits trailing spaces in Swift and Kotlin declarations.
# Normalize only whitespace so tracked bindings remain diff-clean and reproducible.
for binding in \
    "$GEN_DIR/clipboard_core.swift" \
    "$GEN_DIR/uniffi/clipboard_core/clipboard_core.kt" \
    "$ANDROID_GEN_DIR/uniffi/clipboard_core/clipboard_core.kt"; do
    perl -pi -e 's/[ \t]+$//' "$binding"
    perl -0pi -e 's/\n+\z/\n/' "$binding"
done
printf 'PASS: Swift, JVM Kotlin, and Android Kotlin bindings generated\n'

step "Apple XCFramework (pinned Darwin targets, release staticlibs)"
rm -rf "$HEADERS_DIR" "$OUT_DIR/apple"
mkdir -p "$HEADERS_DIR"
cp "$GEN_DIR/clipboard_coreFFI.h" "$HEADERS_DIR/"
cat > "$HEADERS_DIR/module.modulemap" <<'MODULEMAP'
module clipboard_coreFFI {
    header "clipboard_coreFFI.h"
    export *
}
MODULEMAP
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    printf '+ MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release --target %s -p clipboard-core %s\n' "$target" "$CORE_FEATURES"
    MACOSX_DEPLOYMENT_TARGET=13.0 \
        cargo build --locked --release --target "$target" -p clipboard-core $CORE_FEATURES
done
# macOS XCFramework slices are per-platform: merge both pinned Darwin
# targets into one universal archive first.
mkdir -p "$OUT_DIR/apple/universal"
lipo -create \
    "$ROOT_DIR/target/aarch64-apple-darwin/release/libclipboard_core.a" \
    "$ROOT_DIR/target/x86_64-apple-darwin/release/libclipboard_core.a" \
    -output "$OUT_DIR/apple/universal/libclipboard_core.a"
"$XCODEBUILD_BIN" -create-xcframework \
    -library "$OUT_DIR/apple/universal/libclipboard_core.a" -headers "$HEADERS_DIR" \
    -output "$OUT_DIR/apple/ClipboardCore.xcframework"
TRACKED_SWIFT_BINDING="$ROOT_DIR/macos/Sources/ClipboardCoreBindings/clipboard_core.swift"
cmp "$GEN_DIR/clipboard_core.swift" "$TRACKED_SWIFT_BINDING" \
    || fail "tracked Swift binding drifted from the pinned UniFFI output"
MACOS_FRAMEWORK_DIR="$ROOT_DIR/macos/Frameworks"
rm -rf "$MACOS_FRAMEWORK_DIR/ClipboardCore.xcframework"
mkdir -p "$MACOS_FRAMEWORK_DIR"
cp -R "$OUT_DIR/apple/ClipboardCore.xcframework" "$MACOS_FRAMEWORK_DIR/"
printf 'XCFramework slices:\n'
ls "$OUT_DIR/apple/ClipboardCore.xcframework"
for archive in "$OUT_DIR/apple/ClipboardCore.xcframework"/*/libclipboard_core.a; do
    printf '%s: %s\n' "$archive" "$(lipo -info "$archive")"
done
printf 'PASS: XCFramework created with both Darwin slices\n'

step "Android JNI libraries (three pinned ABIs)"
if [[ -n "$ANDROID_UNVERIFIED" ]]; then
    printf 'UNVERIFIED: Android ABI builds skipped — %s\n' "$ANDROID_UNVERIFIED" >&2
else
    rm -rf "$OUT_DIR/android"
    export ANDROID_NDK_HOME="$NDK_DIR"
    printf '+ cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 build --locked --release -p clipboard-core %s\n' "$CORE_FEATURES"
    cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
        -o "$OUT_DIR/android/jniLibs" \
        build --locked --release -p clipboard-core $CORE_FEATURES
    printf 'Android ABI libraries:\n'
    find "$OUT_DIR/android/jniLibs" -name 'libclipboard_core.so' -exec ls -l {} \;
    [[ $(find "$OUT_DIR/android/jniLibs" -name 'libclipboard_core.so' | wc -l) -eq 3 ]] \
        || fail "expected three ABI libraries"
    ANDROID_BINDING_DIR="$ROOT_DIR/android/app/src/main/java/uniffi/clipboard_core"
    ANDROID_JNI_DIR="$ROOT_DIR/android/app/src/main/jniLibs"
    mkdir -p "$ANDROID_BINDING_DIR" "$ANDROID_JNI_DIR"
    cp "$ANDROID_GEN_DIR/uniffi/clipboard_core/clipboard_core.kt" "$ANDROID_BINDING_DIR/clipboard_core.kt"
    rm -rf "$ANDROID_JNI_DIR/arm64-v8a" "$ANDROID_JNI_DIR/armeabi-v7a" "$ANDROID_JNI_DIR/x86_64"
    cp -R "$OUT_DIR/android/jniLibs/arm64-v8a" "$ANDROID_JNI_DIR/"
    cp -R "$OUT_DIR/android/jniLibs/armeabi-v7a" "$ANDROID_JNI_DIR/"
    cp -R "$OUT_DIR/android/jniLibs/x86_64" "$ANDROID_JNI_DIR/"
    printf 'PASS: three Android ABI libraries built\n'
fi

start_relay() {
    cargo build --locked -p clipboard-server
    RELAY_DATA="$(mktemp -d "${TMPDIR:-/tmp}/clipsync-relay-data.XXXXXX")"
    RELAY_MAILBOX="$(mktemp -d "${TMPDIR:-/tmp}/clipsync-relay-mailbox.XXXXXX")"
    RELAY_LOG="$(mktemp "${TMPDIR:-/tmp}/clipsync-relay-log.XXXXXX")"
    BIND_ADDR=127.0.0.1:0 DATA_DIR="$RELAY_DATA" MAILBOX_DIR="$RELAY_MAILBOX" \
        "$ROOT_DIR/target/debug/clipsync-server" 2>"$RELAY_LOG" &
    RELAY_PID=$!
    local deadline=$((SECONDS + 15))
    until grep -q "listening on ws://" "$RELAY_LOG" 2>/dev/null; do
        kill -0 "$RELAY_PID" >/dev/null 2>&1 || fail "relay exited during startup: $(cat "$RELAY_LOG")"
        (( SECONDS < deadline )) || fail "relay did not start within 15s"
        sleep 0.1
    done
    RELAY_URL="$(sed -n 's/.*listening on \(ws:\/\/[^ ]*\).*/\1/p' "$RELAY_LOG" | head -1)"
    printf 'relay: %s\n' "$RELAY_URL"
}

stop_relay() {
    if [[ -n "$RELAY_PID" ]] && kill -0 "$RELAY_PID" >/dev/null 2>&1; then
        kill "$RELAY_PID" >/dev/null 2>&1 || true
        wait "$RELAY_PID" 2>/dev/null || true
    fi
    RELAY_PID=""
}

# The relay rate-limits pairing attempts per minute; each host program gets a
# fresh relay instance.
step "start local relay for host closed loops"
start_relay

step "Swift host closed loop"
mkdir -p "$HOST_DIR"
"$SWIFTC_BIN" \
    "$GEN_DIR/clipboard_core.swift" \
    "$ROOT_DIR/scripts/fixtures/ffi/SwiftClosedLoop.swift" \
    -I "$GEN_DIR" \
    -Xcc "-fmodule-map-file=$GEN_DIR/clipboard_coreFFI.modulemap" \
    -L "$DEBUG_LIB_DIR" \
    -lclipboard_core \
    -Xlinker -rpath -Xlinker "$DEBUG_LIB_DIR" \
    -o "$HOST_DIR/SwiftClosedLoop"
SWIFT_OUTPUT="$(run_with_timeout 60 "$HOST_DIR/SwiftClosedLoop" "$RELAY_URL")"
printf '%s\n' "$SWIFT_OUTPUT"
[[ "$SWIFT_OUTPUT" == *"PASS: Swift host closed loop delivered live clip over UniFFI FFI"* ]] \
    || fail "Swift host closed loop did not report success"
stop_relay
start_relay

step "Swift host deadlock regression (background reset while callback awaits MainActor)"
"$SWIFTC_BIN" \
    "$GEN_DIR/clipboard_core.swift" \
    "$ROOT_DIR/scripts/fixtures/ffi/SwiftDeadlockRegression.swift" \
    -I "$GEN_DIR" \
    -Xcc "-fmodule-map-file=$GEN_DIR/clipboard_coreFFI.modulemap" \
    -L "$DEBUG_LIB_DIR" \
    -lclipboard_core \
    -Xlinker -rpath -Xlinker "$DEBUG_LIB_DIR" \
    -o "$HOST_DIR/SwiftDeadlockRegression"
DEADLOCK_OUTPUT="$(run_with_timeout 60 "$HOST_DIR/SwiftDeadlockRegression" "$RELAY_URL")"
printf '%s\n' "$DEADLOCK_OUTPUT"
[[ "$DEADLOCK_OUTPUT" == *"PASS: background reset_pairing joined while callback awaited MainActor; no deadlock"* ]] \
    || fail "Swift deadlock regression did not report success"
stop_relay
start_relay

step "Kotlin host closed loop (JNA)"
KOTLIN_FIXTURE="$ROOT_DIR/scripts/fixtures/uniffi/kotlin"
mkdir -p "$KOTLIN_FIXTURE/build/generated-binding/uniffi/clipboard_core"
cp "$GEN_DIR/uniffi/clipboard_core/clipboard_core.kt" \
    "$KOTLIN_FIXTURE/build/generated-binding/uniffi/clipboard_core/clipboard_core.kt"
if [[ -z "${JAVA_HOME:-}" ]]; then
    for jdk in \
        "/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home" \
        "/Applications/Android Studio.app/Contents/jbr/Contents/Home"; do
        if [[ -x "$jdk/bin/java" ]]; then
            export JAVA_HOME="$jdk"
            break
        fi
    done
fi
[[ -n "${JAVA_HOME:-}" && -x "$JAVA_HOME/bin/java" ]] || fail "no JDK found for the Kotlin host"
export JAVA_TOOL_OPTIONS="-Dhttp.proxyHost= -Dhttp.proxyPort=80 -Dhttps.proxyHost= -Dhttps.proxyPort=443"
KOTLIN_LOG="$(mktemp "${TMPDIR:-/tmp}/clipsync-kotlin-host.XXXXXX")"
run_with_timeout 300 \
    "$ROOT_DIR/android/gradlew" --no-daemon -p "$KOTLIN_FIXTURE" ffiClosedLoop \
    -PrelayUrl="$RELAY_URL" -PffiLibraryPath="$DEBUG_LIB_DIR" 2>&1 | tee "$KOTLIN_LOG"
grep -q "PASS: Kotlin host closed loop delivered live clip over UniFFI/JNA FFI" "$KOTLIN_LOG" \
    || fail "Kotlin host closed loop did not report success"
grep -q "PASS: Kotlin host stored deferred mailbox as durable RemoteDeferred history" "$KOTLIN_LOG" \
    || fail "Kotlin host did not verify durable deferred mailbox history"

step "summary"
printf 'PASS: ffi feature contract, pinned uniffi 0.29.4, XCFramework slices, Swift+Kotlin host closed loops, durable deferred mailbox history, Swift deadlock regression\n'
if [[ -n "$ANDROID_UNVERIFIED" ]]; then
    printf 'UNVERIFIED: Android three-ABI .so builds — %s\n' "$ANDROID_UNVERIFIED" >&2
fi
