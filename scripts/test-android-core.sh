#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/require-external-dev.sh"
clipsync_require_external_dev "$ROOT_DIR"
ANDROID_DIR="$ROOT_DIR/android"
ARTIFACT_DIR="$CLIPSYNC_TEST_OUTPUT_ROOT/task-14-android"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
export JAVA_TOOL_OPTIONS="-Dhttp.proxyHost= -Dhttp.proxyPort=80 -Dhttps.proxyHost= -Dhttps.proxyPort=443"
export PATH="$HOME/.cargo/bin:$PATH"

ADB="$ANDROID_HOME/platform-tools/adb"
EMULATOR="$ANDROID_HOME/emulator/emulator"
APP_APK_DIR="${CLIPSYNC_ANDROID_BUILD_ROOT:+$CLIPSYNC_ANDROID_BUILD_ROOT/app/outputs/apk}"
APP_APK="${APP_APK_DIR:-$ANDROID_DIR/app/build/outputs/apk}/debug/app-debug.apk"
TEST_APK="${APP_APK_DIR:-$ANDROID_DIR/app/build/outputs/apk}/androidTest/debug/app-debug-androidTest.apk"
RUNNER="com.clipsync.app.test/androidx.test.runner.AndroidJUnitRunner"
CURRENT_SERIAL=""
CURRENT_PID=""

mkdir -p "$ARTIFACT_DIR"

cleanup_emulator() {
    if [[ -n "$CURRENT_SERIAL" ]]; then
        "$ADB" -s "$CURRENT_SERIAL" emu kill >/dev/null 2>&1 || true
    fi
    if [[ -n "$CURRENT_PID" ]] && kill -0 "$CURRENT_PID" >/dev/null 2>&1; then
        kill "$CURRENT_PID" >/dev/null 2>&1 || true
        wait "$CURRENT_PID" 2>/dev/null || true
    fi
    CURRENT_SERIAL=""
    CURRENT_PID=""
}
trap cleanup_emulator EXIT INT TERM

reset_notification_permission() {
    "$ADB" -s "$CURRENT_SERIAL" shell pm revoke \
        com.clipsync.app android.permission.POST_NOTIFICATIONS >/dev/null 2>&1 || true
    "$ADB" -s "$CURRENT_SERIAL" shell pm clear-permission-flags \
        com.clipsync.app android.permission.POST_NOTIFICATIONS user-set
    "$ADB" -s "$CURRENT_SERIAL" shell pm clear-permission-flags \
        com.clipsync.app android.permission.POST_NOTIFICATIONS user-fixed
}

wait_for_boot() {
    local serial="$1"
    local deadline=$((SECONDS + 240))
    "$ADB" -s "$serial" wait-for-device
    while [[ $("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') != "1" ]]; do
        ((SECONDS < deadline)) || { printf 'FAIL: %s boot timeout\n' "$serial"; return 1; }
        sleep 2
    done
    "$ADB" -s "$serial" shell settings put global window_animation_scale 0
    "$ADB" -s "$serial" shell settings put global transition_animation_scale 0
    "$ADB" -s "$serial" shell settings put global animator_duration_scale 0
    "$ADB" -s "$serial" shell input keyevent 82
}

start_emulator() {
    local api="$1"
    local port="$2"
    local avd="clipsync-spike-api$api"
    CURRENT_SERIAL="emulator-$port"
    "$EMULATOR" \
        -avd "$avd" \
        -port "$port" \
        -wipe-data \
        -no-snapshot \
        -no-window \
        -no-audio \
        -no-boot-anim \
        -gpu swiftshader_indirect \
        >"$ARTIFACT_DIR/emulator-api$api.log" 2>&1 &
    CURRENT_PID=$!
    wait_for_boot "$CURRENT_SERIAL"
    [[ $("$ADB" -s "$CURRENT_SERIAL" shell getprop ro.build.version.sdk | tr -d '\r') == "$api" ]]
    printf 'PASS: booted API %s on %s\n' "$api" "$CURRENT_SERIAL"
}

install_apks() {
    "$ADB" -s "$CURRENT_SERIAL" install -r -t "$APP_APK"
    "$ADB" -s "$CURRENT_SERIAL" install -r -t "$TEST_APK"
}

run_instrumentation() {
    local api="$1"
    local label="$2"
    shift 2
    local output
    output="$("$ADB" -s "$CURRENT_SERIAL" shell am instrument -w -r "$@" "$RUNNER" 2>&1)"
    printf '%s\n' "$output" | tee "$ARTIFACT_DIR/api${api}-${label}.log"
    [[ "$output" == *"OK ("* && "$output" != *"FAILURES!!!"* ]]
}

capture_state() {
    local api="$1"
    "$ADB" -s "$CURRENT_SERIAL" shell am start -W -n com.clipsync.app/.MainActivity >/dev/null
    "$ADB" -s "$CURRENT_SERIAL" exec-out uiautomator dump /dev/tty \
        >"$ARTIFACT_DIR/api${api}-status-ui.xml"
    "$ADB" -s "$CURRENT_SERIAL" exec-out screencap -p \
        >"$ARTIFACT_DIR/api${api}-status.png"
    "$ADB" -s "$CURRENT_SERIAL" shell cmd statusbar expand-settings
    sleep 1
    "$ADB" -s "$CURRENT_SERIAL" exec-out uiautomator dump /dev/tty \
        >"$ARTIFACT_DIR/api${api}-tile-ui.xml"
    "$ADB" -s "$CURRENT_SERIAL" exec-out screencap -p \
        >"$ARTIFACT_DIR/api${api}-tile.png"
    "$ADB" -s "$CURRENT_SERIAL" logcat -d >"$ARTIFACT_DIR/api${api}-logcat.txt"
    "$ADB" -s "$CURRENT_SERIAL" logcat -b crash -d >"$ARTIFACT_DIR/api${api}-crash.txt"
}

printf '== Android T14 build gates ==\n'
"$ANDROID_DIR/gradlew" --no-daemon -p "$ANDROID_DIR" \
    --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
    lintDebug testDebugUnitTest assembleDebug assembleDebugAndroidTest --console=plain

printf '== API 29 production flow ==\n'
start_emulator 29 5580
install_apks
"$ADB" -s "$CURRENT_SERIAL" logcat -c
run_instrumentation 29 platform -e class com.clipsync.app.PlatformFeasibilityTest
printf 'QA command: adb -s %s shell cmd statusbar add-tile com.clipsync.app/.ClipSyncTileService\n' "$CURRENT_SERIAL"
printf 'QA result: %s\n' "$("$ADB" -s "$CURRENT_SERIAL" shell cmd statusbar add-tile com.clipsync.app/.ClipSyncTileService)"
capture_state 29
printf 'PASS: API 29 native FFI, FGS, live/mailbox callbacks, tile focus send, image bounds\n'
cleanup_emulator

printf '== API 34 production flow ==\n'
start_emulator 34 5582
install_apks
"$ADB" -s "$CURRENT_SERIAL" logcat -c
"$ADB" -s "$CURRENT_SERIAL" shell pm grant com.clipsync.app android.permission.POST_NOTIFICATIONS
run_instrumentation 34 platform -e class com.clipsync.app.PlatformFeasibilityTest
reset_notification_permission
run_instrumentation 34 notification-grant \
    -e expectedNotificationState visible \
    -e class com.clipsync.app.NotificationPermissionFeasibilityTest
reset_notification_permission
run_instrumentation 34 notification-deny \
    -e expectedNotificationState restricted \
    -e class com.clipsync.app.NotificationPermissionFeasibilityTest
printf 'QA command: adb -s %s shell cmd statusbar add-tile com.clipsync.app/.ClipSyncTileService\n' "$CURRENT_SERIAL"
printf 'QA result: %s\n' "$("$ADB" -s "$CURRENT_SERIAL" shell cmd statusbar add-tile com.clipsync.app/.ClipSyncTileService)"
capture_state 34
printf 'PASS: API 34 PendingIntent tile, notification grant/deny, FGS and deferred mailbox\n'
cleanup_emulator

printf 'PASS: Android T14 dual-API production validation complete\n'
