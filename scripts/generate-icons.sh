#!/usr/bin/env bash
# Renders the ClipSync app icon (assets/icon/*.svg) into all platform assets:
#   macos/Support/AppIcon.icns            (consumed by scripts/package-macos.sh)
#   android/app/src/main/res/mipmap-*/ic_launcher.png
#   android/app/src/main/res/mipmap-*/ic_launcher_foreground.png
#   android/app/src/main/res/mipmap-anydpi-v26/ic_launcher.xml
#   android/app/src/main/res/drawable/ic_launcher_background.xml
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/require-external-dev.sh"
clipsync_require_external_dev "$ROOT_DIR"
ART_DIR="$ROOT_DIR/assets/icon"
MACOS_SUPPORT="$ROOT_DIR/macos/Support"
ANDROID_RES="$ROOT_DIR/android/app/src/main/res"

command -v rsvg-convert >/dev/null 2>&1 || {
    printf 'FAIL: rsvg-convert not found (brew install librsvg)\n' >&2
    exit 1
}
command -v iconutil >/dev/null 2>&1 || {
    printf 'FAIL: iconutil not found\n' >&2
    exit 1
}

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

render() { # render <svg> <size> <output>
    rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
}

# --- macOS .icns -----------------------------------------------------------
ICONSET="$TMP_DIR/AppIcon.iconset"
mkdir -p "$ICONSET"
while read -r name size; do
    [[ -n "$name" ]] || continue
    render "$ART_DIR/app-icon.svg" "$size" "$ICONSET/$name"
done <<'SIZES'
icon_16x16.png 16
icon_16x16@2x.png 32
icon_32x32.png 32
icon_32x32@2x.png 64
icon_128x128.png 128
icon_128x128@2x.png 256
icon_256x256.png 256
icon_256x256@2x.png 512
icon_512x512.png 512
icon_512x512@2x.png 1024
SIZES
iconutil --convert icns "$ICONSET" --output "$MACOS_SUPPORT/AppIcon.icns"

# --- Android mipmaps -------------------------------------------------------
while read -r density legacy foreground; do
    [[ -n "$density" ]] || continue
    out_dir="$ANDROID_RES/mipmap-$density"
    mkdir -p "$out_dir"
    render "$ART_DIR/app-icon.svg" "$legacy" "$out_dir/ic_launcher.png"
    render "$ART_DIR/adaptive-foreground.svg" "$foreground" "$out_dir/ic_launcher_foreground.png"
done <<'DENSITIES'
mdpi 48 108
hdpi 72 162
xhdpi 96 216
xxhdpi 144 324
xxxhdpi 192 432
DENSITIES

mkdir -p "$ANDROID_RES/mipmap-anydpi-v26" "$ANDROID_RES/drawable"
cat > "$ANDROID_RES/mipmap-anydpi-v26/ic_launcher.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">
    <background android:drawable="@drawable/ic_launcher_background" />
    <foreground android:drawable="@mipmap/ic_launcher_foreground" />
</adaptive-icon>
XML
cat > "$ANDROID_RES/drawable/ic_launcher_background.xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<vector xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:aapt="http://schemas.android.com/aapt"
    android:width="108dp"
    android:height="108dp"
    android:viewportWidth="108"
    android:viewportHeight="108">
    <path android:pathData="M0,0h108v108h-108z">
        <aapt:attr name="android:fillColor">
            <gradient
                android:type="linear"
                android:startX="0"
                android:startY="0"
                android:endX="108"
                android:endY="108"
                android:startColor="#4C8DFF"
                android:endColor="#8B5CF6" />
        </aapt:attr>
    </path>
</vector>
XML

printf 'PASS: icons generated -> %s, %s\n' "$MACOS_SUPPORT/AppIcon.icns" "$ANDROID_RES/mipmap-*"
