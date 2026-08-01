#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MACOS_DIR="$ROOT_DIR/macos"
APP_DIR="$ROOT_DIR/dist/spikes/macos/ClipSync.app"
STATUS_FILE="$(mktemp "${TMPDIR:-/tmp}/clipsync-smappservice.XXXXXX")"
APP_PID=""

cleanup() {
    if [[ -n "$APP_PID" ]] && kill -0 "$APP_PID" >/dev/null 2>&1; then
        kill "$APP_PID" >/dev/null 2>&1 || true
        wait "$APP_PID" 2>/dev/null || true
    fi
    rm -f "$STATUS_FILE"
}
trap cleanup EXIT INT TERM

swift build --package-path "$MACOS_DIR" -c release -Xswiftc -warnings-as-errors
BIN_DIR="$(swift build --package-path "$MACOS_DIR" -c release --show-bin-path)"

rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$BIN_DIR/ClipSyncMacOS" "$APP_DIR/Contents/MacOS/ClipSyncMacOS"
cp "$MACOS_DIR/Support/Info.plist" "$APP_DIR/Contents/Info.plist"
/usr/bin/codesign --force --sign - "$APP_DIR"
/usr/bin/codesign --verify --strict "$APP_DIR"
printf 'PASS: packaged and ad-hoc signed %s\n' "$APP_DIR"

rm -f "$STATUS_FILE"
CLIPSYNC_SPIKE_STATUS_FILE="$STATUS_FILE" "$APP_DIR/Contents/MacOS/ClipSyncMacOS" &
APP_PID=$!
deadline=$((SECONDS + 10))
while [[ ! -s "$STATUS_FILE" ]]; do
    if ! kill -0 "$APP_PID" >/dev/null 2>&1; then
        wait "$APP_PID" || true
        printf 'FAIL: packaged ClipSync.app exited before reporting SMAppService registration\n' >&2
        exit 1
    fi
    if (( SECONDS >= deadline )); then
        printf 'FAIL: packaged ClipSync.app did not report SMAppService registration within 10 seconds\n' >&2
        exit 1
    fi
    sleep 0.1
done

STATUS="$(<"$STATUS_FILE")"
[[ "$STATUS" == "SMAppService registration call completed:"* ]] || {
    printf 'FAIL: unexpected SMAppService marker: %s\n' "$STATUS" >&2
    exit 1
}
kill -0 "$APP_PID"
printf 'PASS: packaged ClipSync.app launched and remained alive; %s\n' "$STATUS"
kill "$APP_PID"
wait "$APP_PID" 2>/dev/null || true
APP_PID=""

PROBE_OUTPUT="$($BIN_DIR/ClipSyncDeadlockProbe)"
printf '%s\n' "$PROBE_OUTPUT"
[[ "$PROBE_OUTPUT" == *"PASS: queued callback and background shutdown completed without blocking main thread"* ]] || {
    printf 'FAIL: deadlock probe did not report success\n' >&2
    exit 1
}
printf 'PASS: all macOS platform feasibility spikes completed\n'
