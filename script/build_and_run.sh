#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
APP_NAME="ClipSyncMacOS"
BUNDLE_ID="com.clipsync.macos"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MACOS_DIR="$ROOT_DIR/macos"
APP_BUNDLE="$ROOT_DIR/dist/dev/ClipboardSync.app"
APP_BINARY="$APP_BUNDLE/Contents/MacOS/$APP_NAME"

for pid in $(pgrep -f "^${APP_BINARY}$" || true); do
    kill "$pid" >/dev/null 2>&1 || true
done

swift build --package-path "$MACOS_DIR" -Xswiftc -warnings-as-errors
BIN_DIR="$(swift build --package-path "$MACOS_DIR" --show-bin-path)"

rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
cp "$BIN_DIR/$APP_NAME" "$APP_BINARY"
cp "$MACOS_DIR/Support/Info.plist" "$APP_BUNDLE/Contents/Info.plist"
chmod +x "$APP_BINARY"
/usr/bin/codesign --force --sign - "$APP_BUNDLE"

open_app() {
    /usr/bin/open -n "$APP_BUNDLE"
}

case "$MODE" in
    run)
        open_app
        ;;
    --debug|debug)
        /usr/bin/lldb -- "$APP_BINARY"
        ;;
    --logs|logs)
        open_app
        /usr/bin/log stream --info --style compact --predicate "process == \"$APP_NAME\""
        ;;
    --telemetry|telemetry)
        open_app
        /usr/bin/log stream --info --style compact --predicate "subsystem == \"$BUNDLE_ID\""
        ;;
    --verify|verify)
        open_app
        sleep 1
        pgrep -f "^${APP_BINARY}$" >/dev/null
        ;;
    *)
        printf 'usage: %s [run|--debug|--logs|--telemetry|--verify]\n' "$0" >&2
        exit 2
        ;;
esac
