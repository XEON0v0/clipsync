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
    export CLIPSYNC_JAVA_TMPDIR="$CLIPSYNC_PROJECT_BUILD_ROOT/temp/java"
    export CLIPSYNC_KOTLIN_DAEMON_RUN_FILES_PATH="$CLIPSYNC_PROJECT_BUILD_ROOT/temp/kotlin-daemon"
    export ANDROID_TMP="$CLIPSYNC_PROJECT_BUILD_ROOT/temp/android"

    /bin/mkdir -p \
        "$CLIPSYNC_JAVA_TMPDIR" \
        "$CLIPSYNC_KOTLIN_DAEMON_RUN_FILES_PATH" \
        "$ANDROID_TMP"

    typeset -a clipsync_java_options=()
    typeset -a clipsync_preserved_java_options=()
    typeset -a clipsync_kotlin_daemon_options=()
    typeset clipsync_java_option="" clipsync_kotlin_daemon_option=""
    if [[ -n "${JAVA_TOOL_OPTIONS:-}" ]]; then
        clipsync_java_options=(${(z)JAVA_TOOL_OPTIONS})
    fi
    for clipsync_java_option in "${clipsync_java_options[@]}"; do
        case "$clipsync_java_option" in
            -Djava.io.tmpdir=*) ;;
            -Dkotlin.daemon.options=*)
                for clipsync_kotlin_daemon_option \
                    in ${(s:,:)${clipsync_java_option#-Dkotlin.daemon.options=}}; do
                    [[ -n "$clipsync_kotlin_daemon_option" \
                        && "$clipsync_kotlin_daemon_option" != runFilesPath=* ]] \
                        && clipsync_kotlin_daemon_options+=("$clipsync_kotlin_daemon_option")
                done
                ;;
            *) clipsync_preserved_java_options+=("$clipsync_java_option") ;;
        esac
    done
    clipsync_kotlin_daemon_options+=(
        "runFilesPath=$CLIPSYNC_KOTLIN_DAEMON_RUN_FILES_PATH"
    )
    clipsync_preserved_java_options+=(
        "-Djava.io.tmpdir=$CLIPSYNC_JAVA_TMPDIR"
        "-Dkotlin.daemon.options=${(j:,:)clipsync_kotlin_daemon_options}"
    )
    export JAVA_TOOL_OPTIONS="${(j: :)clipsync_preserved_java_options}"

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
