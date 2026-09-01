#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/require-external-dev.sh"
clipsync_require_external_dev "$ROOT_DIR"
MACOS_DIR="$ROOT_DIR/macos"
APP_BUNDLE="$CLIPSYNC_RELEASE_OUTPUT_ROOT/ClipboardSync.app"
APP_BINARY="$APP_BUNDLE/Contents/MacOS/ClipSyncMacOS"
OPEN_PID=""
APP_PID=""

cleanup() {
    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" >/dev/null 2>&1; then
        kill "$APP_PID" >/dev/null 2>&1 || true
    fi
    if [[ -n "$OPEN_PID" ]]; then
        wait "$OPEN_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

SWIFT_BUILD=(swift build --package-path "$MACOS_DIR" \
    --scratch-path "$CLIPSYNC_SWIFTPM_SCRATCH" \
    --cache-path "$CLIPSYNC_SWIFTPM_CACHE")
"${SWIFT_BUILD[@]}" -c release -Xswiftc -warnings-as-errors
BIN_DIR="$("${SWIFT_BUILD[@]}" -c release --show-bin-path)"

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
cp "$BIN_DIR/ClipSyncMacOS" "$APP_BINARY"
cp "$MACOS_DIR/Support/Info.plist" "$APP_BUNDLE/Contents/Info.plist"
cp "$MACOS_DIR/Support/AppIcon.icns" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
chmod +x "$APP_BINARY"

/usr/bin/codesign --force --sign - "$APP_BUNDLE"
/usr/bin/codesign --verify --strict "$APP_BUNDLE"
/usr/bin/codesign --display --verbose=2 "$APP_BUNDLE"

/usr/bin/open -n -W "$APP_BUNDLE" &
OPEN_PID=$!
deadline=$((SECONDS + 10))
while [[ -z "$APP_PID" ]]; do
    APP_PID="$(pgrep -f "^${APP_BINARY}$" | head -1 || true)"
    if ! kill -0 "$OPEN_PID" >/dev/null 2>&1; then
        wait "$OPEN_PID" || true
        printf 'FAIL: packaged app exited during launch\n' >&2
        exit 1
    fi
    if (( SECONDS >= deadline )); then
        printf 'FAIL: packaged app did not launch within 10 seconds\n' >&2
        exit 1
    fi
    sleep 0.1
done
sleep 1
kill -0 "$APP_PID"
printf 'PASS: packaged, ad-hoc signed, and launched %s\n' "$APP_BUNDLE"
