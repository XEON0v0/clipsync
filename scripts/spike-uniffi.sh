#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_HTTP_PROXY=""
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://mirrors.ustc.edu.cn/rust-static}"
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy || true
OUT_DIR="$ROOT_DIR/dist/spikes/uniffi"
LIB_DIR="$ROOT_DIR/target/debug"
LIBRARY="$LIB_DIR/libclipboard_core.dylib"

cd "$ROOT_DIR"
VERSION_OUTPUT="$(cargo run --locked -p clipsync-uniffi-bindgen -- --version)"
printf '%s\n' "$VERSION_OUTPUT"
[[ "$VERSION_OUTPUT" == "uniffi-bindgen 0.29.4" ]] || {
    printf 'FAIL: expected exact UniFFI version 0.29.4\n' >&2
    exit 1
}
printf 'PASS: workspace wrapper reports exact uniffi-bindgen 0.29.4\n'

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
cargo fetch --locked
cargo build --locked -p clipboard-core --features full
[[ -f "$LIBRARY" ]] || { printf 'FAIL: clipboard-core dylib was not built\n' >&2; exit 1; }

cargo run --locked -p clipsync-uniffi-bindgen -- generate \
    --library "$LIBRARY" \
    --language swift \
    --language kotlin \
    --out-dir "$OUT_DIR"

SWIFT_BINDING="$OUT_DIR/clipboard_core.swift"
SWIFT_HEADER="$OUT_DIR/clipboard_coreFFI.h"
SWIFT_MODULEMAP="$OUT_DIR/clipboard_coreFFI.modulemap"
KOTLIN_BINDING="$OUT_DIR/uniffi/clipboard_core/clipboard_core.kt"
[[ -f "$SWIFT_BINDING" && -f "$SWIFT_HEADER" && -f "$SWIFT_MODULEMAP" ]] || {
    printf 'FAIL: Swift binding artifacts are incomplete\n' >&2
    exit 1
}
[[ -f "$KOTLIN_BINDING" ]] || {
    printf 'FAIL: Kotlin binding was not generated at %s\n' "$KOTLIN_BINDING" >&2
    exit 1
}
printf 'PASS: generated Swift and Kotlin bindings through the workspace wrapper\n'

swiftc \
    "$SWIFT_BINDING" \
    "$ROOT_DIR/scripts/fixtures/uniffi/CallbackHost.swift" \
    -I "$OUT_DIR" \
    -Xcc "-fmodule-map-file=$SWIFT_MODULEMAP" \
    -L "$LIB_DIR" \
    -lclipboard_core \
    -Xlinker -rpath \
    -Xlinker "$LIB_DIR" \
    -o "$OUT_DIR/CallbackHost"

CALLBACK_OUTPUT="$($OUT_DIR/CallbackHost)"
printf '%s\n' "$CALLBACK_OUTPUT"
[[ "$CALLBACK_OUTPUT" == "PASS: Swift host received Rust-to-foreign callback: swift-to-rust-to-swift" ]] || {
    printf 'FAIL: foreign callback host did not complete the expected round trip\n' >&2
    exit 1
}

KOTLIN_FIXTURE="$ROOT_DIR/scripts/fixtures/uniffi/kotlin"
mkdir -p "$KOTLIN_FIXTURE/build/generated-binding/uniffi/clipboard_core"
cp "$KOTLIN_BINDING" "$KOTLIN_FIXTURE/build/generated-binding/uniffi/clipboard_core/clipboard_core.kt"
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
export JAVA_TOOL_OPTIONS="-Dhttp.proxyHost= -Dhttp.proxyPort=80 -Dhttps.proxyHost= -Dhttps.proxyPort=443"
"$ROOT_DIR/android/gradlew" --no-daemon -p "$KOTLIN_FIXTURE" compileKotlin
printf 'PASS: generated Kotlin binding compiles against the JNA runtime\n'

for target in \
    x86_64-apple-darwin \
    aarch64-linux-android \
    armv7-linux-androideabi \
    x86_64-linux-android; do
    env \
        -u http_proxy -u https_proxy -u HTTP_PROXY -u HTTPS_PROXY -u ALL_PROXY -u all_proxy \
        /opt/homebrew/bin/rustup target add "$target"
    INSTALLED_TARGETS="$(/opt/homebrew/bin/rustup target list --installed)"
    [[ $'\n'"$INSTALLED_TARGETS"$'\n' == *$'\n'"$target"$'\n'* ]] || {
        printf 'FAIL: rustup did not report target as installed: %s\n' "$target" >&2
        exit 1
    }
    printf 'PASS: rustup target available: %s\n' "$target"
done

printf 'PASS: all UniFFI platform feasibility spikes completed\n'
