#!/bin/zsh

typeset clipsync_user_env="${XDG_CONFIG_HOME:-$HOME/.config}/clipsync/external-dev-env.zsh"
if [[ -f "$clipsync_user_env" ]]; then
    source "$clipsync_user_env"
else
    export CLIPSYNC_EXTERNAL_READY=0
fi

export CLIPSYNC_EXTERNAL_LINKS_READY=0
if [[ "$CLIPSYNC_EXTERNAL_READY" == 1 ]]; then
    typeset clipsync_script_path="${(%):-%N}"
    typeset clipsync_workspace_root="${clipsync_script_path:A:h:h}"
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
