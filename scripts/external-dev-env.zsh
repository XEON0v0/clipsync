#!/bin/zsh

typeset external_dev_user_env="$HOME/.config/external-dev/env.zsh"
typeset clipsync_user_env="${XDG_CONFIG_HOME:-$HOME/.config}/clipsync/external-dev-env.zsh"
if [[ -f "$external_dev_user_env" ]]; then
    source "$external_dev_user_env"
elif [[ -f "$clipsync_user_env" ]]; then
    source "$clipsync_user_env"
else
    export CLIPSYNC_EXTERNAL_READY=0
    export EXTERNAL_DEV_READY=0
fi

export CLIPSYNC_EXTERNAL_LINKS_READY=0
if [[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]; then
    typeset clipsync_script_path="${(%):-%N}"
    typeset clipsync_workspace_root="${clipsync_script_path:A:h:h}"
    export CLIPSYNC_PROJECT_BUILD_ROOT="$(external_dev_project_root "$clipsync_workspace_root")"
    export CLIPSYNC_RELEASE_OUTPUT_ROOT="${CLIPSYNC_RELEASE_OUTPUT_ROOT:-$EXTERNAL_DEV_ARTIFACT_ROOT/ClipSync}"
    export SWIFTPM_BUILD_DIR="$CLIPSYNC_PROJECT_BUILD_ROOT/swift"
    typeset clipsync_framework_target="$CLIPSYNC_DEV_VOLUME/ClipSyncBuild/Frameworks"
    typeset clipsync_framework_link="$clipsync_workspace_root/macos/Frameworks"

    /bin/mkdir -p "$clipsync_framework_target"
    if [[ ! -e "$clipsync_framework_link" && ! -L "$clipsync_framework_link" ]]; then
        /bin/ln -s "$clipsync_framework_target" "$clipsync_framework_link"
    fi

    if [[ -L "$clipsync_framework_link" \
        && "$(/usr/bin/readlink "$clipsync_framework_link")" == "$clipsync_framework_target" ]]; then
        export CLIPSYNC_EXTERNAL_LINKS_READY=1
    fi
fi
