#!/bin/zsh
set -euo pipefail

typeset script_path="${(%):-%N}"
typeset workspace_root="${script_path:A:h:h}"
source "$workspace_root/scripts/external-dev-env.zsh"

fail() {
    print -u2 -r -- "FAIL: $*"
    exit 1
}

[[ "$EXTERNAL_DEV_READY" == 1 ]] || fail "external development volume is not ready"
[[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]] || fail "ClipSync external environment is not ready"
[[ "$CLIPSYNC_EXTERNAL_LINKS_READY" == 1 ]] || fail "ClipSync framework link is not ready"
[[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" \
    | plutil -extract FilesystemType raw -o - -)" == apfs ]] \
    || fail "development volume is not APFS"

typeset -a required_external_paths=(
    "$ANDROID_HOME"
    "$ANDROID_AVD_HOME"
    "$GRADLE_USER_HOME"
    "$CARGO_HOME"
    "$RUSTUP_HOME"
    "$CARGO_TARGET_DIR"
    "$SWIFTPM_BUILD_DIR"
    "$KONAN_DATA_DIR"
    "$GOMODCACHE"
    "$GOCACHE"
    "$XDG_CACHE_HOME"
    "$npm_config_cache"
    "$npm_config_devdir"
    "$PNPM_STORE_PATH"
    "$BUN_INSTALL_CACHE_DIR"
    "$PIP_CACHE_DIR"
    "$UV_CACHE_DIR"
    "$PLAYWRIGHT_BROWSERS_PATH"
    "$TMPDIR"
    "$CLIPSYNC_PROJECT_BUILD_ROOT"
    "$CLIPSYNC_RELEASE_OUTPUT_ROOT"
)

typeset checked_path
for checked_path in "${required_external_paths[@]}"; do
    [[ "$checked_path" == "$EXTERNAL_DEV_VOLUME"/* ]] \
        || fail "path escapes external volume: $checked_path"
done

typeset -a compatibility_links=(
    "$HOME/Library/Android/sdk"
    "$HOME/.gradle"
    "$HOME/.cargo"
    "$HOME/.rustup"
    "$HOME/.colima"
    "$HOME/.cache"
    "$HOME/.npm"
    "$HOME/.m2"
    "$HOME/.konan"
    "$HOME/.bun"
    "$HOME/.nuget"
    "$HOME/.android"
    "$HOME/.local/share/uv"
    "$HOME/Library/pnpm"
    "$HOME/Library/Caches/Homebrew"
    "$HOME/Library/Caches/pip"
    "$HOME/Library/Caches/org.swift.swiftpm"
    "$HOME/Library/Caches/ms-playwright"
    "$HOME/Library/Caches/com.apple.dt.Xcode"
    "$HOME/Library/Caches/Google"
    "$HOME/Library/Caches/bun"
    "$HOME/Library/Caches/typescript"
    "$HOME/Library/Caches/node-gyp"
    "$HOME/Library/Caches/electron"
    "$HOME/Library/Caches/pnpm"
    "$HOME/Library/Caches/colima"
    "$HOME/Library/Developer/Xcode/DerivedData"
    "$HOME/Library/Developer/CoreSimulator"
)

for checked_path in "${compatibility_links[@]}"; do
    [[ -L "$checked_path" ]] \
        || fail "compatibility path is not a symlink: $checked_path"
    [[ "${checked_path:A}" == "$EXTERNAL_DEV_VOLUME"/* ]] \
        || fail "compatibility link is not external: $checked_path -> ${checked_path:A}"
done

typeset leaked
leaked="$(find "$workspace_root" -xdev \
    \( -path "$workspace_root/.git" -o -path "$workspace_root/dist" \) -prune -o \
    -type d \( -name target -o -name build -o -name .build -o -name .gradle \
        -o -name DerivedData -o -name .cxx -o -name .externalNativeBuild \) \
    -print)"
[[ -z "$leaked" ]] || fail "internal build directories found:\n$leaked"

print -r -- "PASS: external APFS development environment and compatibility links verified"
