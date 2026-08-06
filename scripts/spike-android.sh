#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_DIR="$ROOT_DIR/android"
export PATH="$HOME/.cargo/bin:$PATH"
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export JAVA_HOME="${JAVA_HOME:-/Applications/IntelliJ IDEA.app/Contents/jbr/Contents/Home}"
export JAVA_TOOL_OPTIONS="-Dhttp.proxyHost= -Dhttp.proxyPort=80 -Dhttps.proxyHost= -Dhttps.proxyPort=443"

ADB="$ANDROID_HOME/platform-tools/adb"
AVDMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager"
EMULATOR="$ANDROID_HOME/emulator/emulator"
AAPT2="$ANDROID_HOME/build-tools/34.0.0/aapt2"
GRADLE_PROXY_ARGS=(-Dhttp.proxyHost= -Dhttp.proxyPort=80 -Dhttps.proxyHost= -Dhttps.proxyPort=443)
APP_APK_DIR="${CLIPSYNC_ANDROID_BUILD_ROOT:+$CLIPSYNC_ANDROID_BUILD_ROOT/app/outputs/apk}"
APP_APK="${APP_APK_DIR:-$ANDROID_DIR/app/build/outputs/apk}/debug/app-debug.apk"
TEST_APK="${APP_APK_DIR:-$ANDROID_DIR/app/build/outputs/apk}/androidTest/debug/app-debug-androidTest.apk"
RUNNER="com.clipsync.app.test/androidx.test.runner.AndroidJUnitRunner"

CURRENT_PID=""
CURRENT_SERIAL=""
SCRATCH_DIR=""

cleanup_emulator() {
    if [[ -n "$CURRENT_SERIAL" ]]; then
        "$ADB" -s "$CURRENT_SERIAL" emu kill >/dev/null 2>&1 || true
    fi
    if [[ -n "$CURRENT_PID" ]] && kill -0 "$CURRENT_PID" >/dev/null 2>&1; then
        kill "$CURRENT_PID" >/dev/null 2>&1 || true
        wait "$CURRENT_PID" 2>/dev/null || true
    fi
    CURRENT_PID=""
    CURRENT_SERIAL=""
}

cleanup_all() {
    cleanup_emulator
    if [[ -n "$SCRATCH_DIR" ]]; then
        rm -rf "$SCRATCH_DIR"
    fi
}
trap cleanup_all EXIT INT TERM

create_avd() {
    local api="$1"
    local name="clipsync-spike-api$api"
    local image="system-images;android-$api;google_apis;arm64-v8a"
    local avds
    avds="$($EMULATOR -list-avds)"
    if [[ $'\n'"$avds"$'\n' != *$'\n'"$name"$'\n'* ]]; then
        printf 'no\n' | "$AVDMANAGER" create avd --force --name "$name" --package "$image" --device pixel_2 >&2
    fi
    printf '%s\n' "$name"
}

wait_for_boot() {
    local serial="$1"
    local deadline=$((SECONDS + 240))
    "$ADB" -s "$serial" wait-for-device
    while [[ $("$ADB" -s "$serial" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r') != "1" ]]; do
        if (( SECONDS >= deadline )); then
            printf 'FAIL: emulator %s did not boot within 240 seconds\n' "$serial" >&2
            "$ADB" -s "$serial" shell getprop >&2 || true
            return 1
        fi
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
    local name
    name="$(create_avd "$api")"
    CURRENT_SERIAL="emulator-$port"
    "$EMULATOR" \
        -avd "$name" \
        -port "$port" \
        -wipe-data \
        -no-snapshot \
        -no-window \
        -no-audio \
        -no-boot-anim \
        -gpu swiftshader_indirect \
        >"${TMPDIR:-/tmp}/clipsync-emulator-api${api}.log" 2>&1 &
    CURRENT_PID=$!
    wait_for_boot "$CURRENT_SERIAL"
    local actual_api
    actual_api="$("$ADB" -s "$CURRENT_SERIAL" shell getprop ro.build.version.sdk | tr -d '\r')"
    [[ "$actual_api" == "$api" ]] || { printf 'FAIL: requested API %s, booted API %s\n' "$api" "$actual_api" >&2; return 1; }
    printf 'PASS: booted API %s on %s\n' "$api" "$CURRENT_SERIAL"
}

install_apks() {
    local serial="$1"
    "$ADB" -s "$serial" install -r -t "$APP_APK"
    "$ADB" -s "$serial" install -r -t "$TEST_APK"
}

run_instrumentation() {
    local serial="$1"
    shift
    local output
    output="$("$ADB" -s "$serial" shell am instrument -w -r "$@" "$RUNNER" 2>&1)"
    printf '%s\n' "$output"
    [[ "$output" == *"OK ("* ]] && [[ "$output" != *"FAILURES!!!"* ]]
}

run_api29() {
    start_emulator 29 5580
    install_apks "$CURRENT_SERIAL"
    run_instrumentation "$CURRENT_SERIAL" -e class com.clipsync.app.PlatformFeasibilityTest
    printf 'PASS: API 29 background-read restriction, FGS write, and real Intent tile path\n'
    cleanup_emulator
}

run_notification_state() {
    local serial="$1"
    local expected="$2"
    run_instrumentation "$serial" \
        -e expectedNotificationState "$expected" \
        -e class com.clipsync.app.NotificationPermissionFeasibilityTest
    printf 'PASS: API 34 notification state %s keeps FGS running and is visible in app status\n' "$expected"
}

run_focus_failure_injection() {
    local serial="$1"
    SCRATCH_DIR="$(mktemp -d "${TMPDIR:-/tmp}/clipsync-focus-failure.XXXXXX")"
    rsync -a --exclude .gradle --exclude build --exclude '*/build' "$ANDROID_DIR/" "$SCRATCH_DIR/android/"
    perl -0pi -e 's/override fun onWindowFocusChanged\(hasFocus: Boolean\) \{\n        super\.onWindowFocusChanged\(hasFocus\)\n        if \(hasFocus\) \{\n            recordClipboardRead\("window_focus"\)\n        \}\n    \}/override fun onResume() {\n        super.onResume()\n        recordClipboardRead("on_resume")\n    }\n\n    override fun onWindowFocusChanged(hasFocus: Boolean) {\n        super.onWindowFocusChanged(hasFocus)\n    }/' \
        "$SCRATCH_DIR/android/app/src/main/java/com/clipsync/app/FocusClipboardActivity.kt"
    if [[ "$(<"$SCRATCH_DIR/android/app/src/main/java/com/clipsync/app/FocusClipboardActivity.kt")" != *'recordClipboardRead("on_resume")'* ]]; then
        printf 'FAIL: failure injection did not move the read to onResume\n' >&2
        return 1
    fi
    env -u CLIPSYNC_ANDROID_BUILD_ROOT "$SCRATCH_DIR/android/gradlew" --no-daemon "${GRADLE_PROXY_ARGS[@]}" \
        -p "$SCRATCH_DIR/android" :app:assembleDebug :app:assembleDebugAndroidTest
    "$ADB" -s "$serial" install -r -t "$SCRATCH_DIR/android/app/build/outputs/apk/debug/app-debug.apk"
    "$ADB" -s "$serial" install -r -t "$SCRATCH_DIR/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"

    local output
    output="$("$ADB" -s "$serial" shell am instrument -w -r \
        -e class 'com.clipsync.app.PlatformFeasibilityTest#realSystemTilePendingIntentOverloadReadsClipboardAfterWindowFocus' \
        "$RUNNER" 2>&1)"
    printf '%s\n' "$output"
    if [[ "$output" == *"OK ("* ]] || [[ "$output" != *"FAILURES!!!"* ]]; then
        printf 'FAIL: onResume failure injection did not make instrumentation RED\n' >&2
        return 1
    fi
    printf 'PASS: onResume failure injection made the focus-timing instrumentation RED\n'
    rm -rf "$SCRATCH_DIR"
    SCRATCH_DIR=""
}

run_api34() {
    start_emulator 34 5582
    install_apks "$CURRENT_SERIAL"
    "$ADB" -s "$CURRENT_SERIAL" shell pm grant com.clipsync.app android.permission.POST_NOTIFICATIONS
    run_instrumentation "$CURRENT_SERIAL" -e class com.clipsync.app.PlatformFeasibilityTest
    printf 'PASS: API 34 background-read restriction, FGS write, and both real tile overloads\n'
    run_notification_state "$CURRENT_SERIAL" visible
    "$ADB" -s "$CURRENT_SERIAL" shell pm revoke com.clipsync.app android.permission.POST_NOTIFICATIONS
    run_notification_state "$CURRENT_SERIAL" restricted
    run_focus_failure_injection "$CURRENT_SERIAL"
    cleanup_emulator
}

run_api35() {
    start_emulator 35 5584
    install_apks "$CURRENT_SERIAL"
    run_instrumentation "$CURRENT_SERIAL" -e class com.clipsync.app.Api35CompatibilityTest
    printf 'PASS: API 35 boots and runs the target-34 dataSync FGS\n'
    cleanup_emulator
}

printf '== Build Android spike APKs ==\n'
"$ANDROID_DIR/gradlew" --no-daemon "${GRADLE_PROXY_ARGS[@]}" \
    -p "$ANDROID_DIR" :app:assembleDebug :app:assembleDebugAndroidTest

BADGING="$("$AAPT2" dump badging "$APP_APK")"
printf '%s\n' "$BADGING"
[[ "$BADGING" == *"sdkVersion:'29'"* ]] || { printf 'FAIL: APK minSdk is not 29\n' >&2; exit 1; }
[[ "$BADGING" == *"targetSdkVersion:'34'"* ]] || { printf 'FAIL: APK targetSdk is not 34\n' >&2; exit 1; }
printf 'PASS: APK manifest reports minSdk 29 / targetSdk 34\n'

run_api29
run_api34
run_api35
printf 'PASS: all Android platform feasibility spikes completed\n'
