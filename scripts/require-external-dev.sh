#!/usr/bin/env bash

# Source this file, then call clipsync_require_external_dev with the workspace
# root. The global environment is deliberately bash/zsh compatible.
clipsync_require_external_dev() {
    local workspace_root="$1"
    local global_env="$HOME/.config/external-dev/env.zsh"

    if [[ ! -f "$global_env" ]]; then
        printf 'FAIL: external development environment is missing: %s\n' "$global_env" >&2
        return 1
    fi
    # shellcheck source=/dev/null
    source "$global_env"
    if [[ "${EXTERNAL_DEV_READY:-0}" != 1 \
        || "${CLIPSYNC_EXTERNAL_READY:-0}" != 1 \
        || ! -w "${EXTERNAL_DEV_VOLUME:-/nonexistent}" ]]; then
        printf 'FAIL: external APFS development storage is unavailable\n' >&2
        return 1
    fi
    if [[ "$(diskutil info -plist "$EXTERNAL_DEV_VOLUME" \
        | plutil -extract FilesystemType raw -o - -)" != apfs ]]; then
        printf 'FAIL: development storage is not APFS: %s\n' "$EXTERNAL_DEV_VOLUME" >&2
        return 1
    fi

    export CLIPSYNC_PROJECT_BUILD_ROOT="$(external_dev_project_root "$workspace_root")"
    export CLIPSYNC_RELEASE_OUTPUT_ROOT="${CLIPSYNC_RELEASE_OUTPUT_ROOT:-$EXTERNAL_DEV_ARTIFACT_ROOT/ClipSync}"
    export CLIPSYNC_SWIFTPM_SCRATCH="$CLIPSYNC_PROJECT_BUILD_ROOT/swift"
    export CLIPSYNC_SWIFTPM_CACHE="$EXTERNAL_DEV_CACHE_ROOT/swiftpm"
    export CLIPSYNC_TEST_OUTPUT_ROOT="$CLIPSYNC_PROJECT_BUILD_ROOT/test-results"
    export SWIFTPM_BUILD_DIR="$CLIPSYNC_SWIFTPM_SCRATCH"
    mkdir -p \
        "$CLIPSYNC_PROJECT_BUILD_ROOT" \
        "$CLIPSYNC_RELEASE_OUTPUT_ROOT" \
        "$CLIPSYNC_SWIFTPM_SCRATCH" \
        "$CLIPSYNC_SWIFTPM_CACHE" \
        "$CLIPSYNC_TEST_OUTPUT_ROOT"
}
