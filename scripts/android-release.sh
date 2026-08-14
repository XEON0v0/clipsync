#!/usr/bin/env bash
# Generates/reuses the local release key and emits a verified signed APK.
set +x
set -euo pipefail
umask 077

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/require-external-dev.sh"
clipsync_require_external_dev "$ROOT_DIR"
ANDROID_DIR="$ROOT_DIR/android"
KEYSTORE_DIR="$ANDROID_DIR/keystore"
KEYSTORE_FILE="$KEYSTORE_DIR/release.jks"
PROPERTIES_FILE="$KEYSTORE_DIR/keystore.properties"
DIST_FILE="$CLIPSYNC_RELEASE_OUTPUT_ROOT/clipboard-sync.apk"
KEY_ALIAS="clipsync-release"

cleanup() {
    unset CLIPSYNC_STORE_PASSWORD CLIPSYNC_KEY_PASSWORD STORE_PASSWORD KEY_PASSWORD
}
trap cleanup EXIT INT TERM

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

property_value() {
    local key="$1"
    sed -n "s/^${key}=//p" "$PROPERTIES_FILE" | head -1
}

KEYTOOL_BIN="$(command -v keytool)" || fail "keytool not found"
OPENSSL_BIN="$(command -v openssl)" || fail "openssl not found"
SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
APKSIGNER_BIN=""
for candidate in \
    "$SDK_ROOT/build-tools/34.0.0/apksigner" \
    "$SDK_ROOT/build-tools/35.0.0/apksigner" \
    "$SDK_ROOT/build-tools/36.1.0/apksigner"; do
    if [[ -x "$candidate" ]]; then
        APKSIGNER_BIN="$candidate"
        break
    fi
done
[[ -n "$APKSIGNER_BIN" ]] || fail "apksigner not found in Android build-tools"

mkdir -p "$KEYSTORE_DIR" "$CLIPSYNC_RELEASE_OUTPUT_ROOT"
chmod 700 "$KEYSTORE_DIR"

if [[ -f "$KEYSTORE_FILE" || -f "$PROPERTIES_FILE" ]]; then
    [[ -f "$KEYSTORE_FILE" && -f "$PROPERTIES_FILE" ]] \
        || fail "release.jks and keystore.properties must exist together"
    chmod 600 "$KEYSTORE_FILE" "$PROPERTIES_FILE"
    CLIPSYNC_STORE_PASSWORD="$(property_value storePassword)"
    CLIPSYNC_KEY_PASSWORD="$(property_value keyPassword)"
    KEY_ALIAS="$(property_value keyAlias)"
    [[ -n "$CLIPSYNC_STORE_PASSWORD" && -n "$CLIPSYNC_KEY_PASSWORD" && -n "$KEY_ALIAS" ]] \
        || fail "keystore.properties is incomplete"
else
    CLIPSYNC_STORE_PASSWORD="$($OPENSSL_BIN rand -hex 32)"
    CLIPSYNC_KEY_PASSWORD="$($OPENSSL_BIN rand -hex 32)"
    export CLIPSYNC_STORE_PASSWORD CLIPSYNC_KEY_PASSWORD
    "$KEYTOOL_BIN" -genkeypair \
        -storetype JKS \
        -keystore "$KEYSTORE_FILE" \
        -storepass:env CLIPSYNC_STORE_PASSWORD \
        -keypass:env CLIPSYNC_KEY_PASSWORD \
        -alias "$KEY_ALIAS" \
        -keyalg RSA \
        -keysize 3072 \
        -validity 10000 \
        -dname "CN=ClipSync Release, OU=Local, O=ClipSync, C=CN"
    {
        printf 'storeFile=keystore/release.jks\n'
        printf 'storePassword=%s\n' "$CLIPSYNC_STORE_PASSWORD"
        printf 'keyAlias=%s\n' "$KEY_ALIAS"
        printf 'keyPassword=%s\n' "$CLIPSYNC_KEY_PASSWORD"
    } > "$PROPERTIES_FILE"
fi

chmod 600 "$KEYSTORE_FILE" "$PROPERTIES_FILE"
export CLIPSYNC_STORE_PASSWORD CLIPSYNC_KEY_PASSWORD

"$KEYTOOL_BIN" -list \
    -storetype JKS \
    -keystore "$KEYSTORE_FILE" \
    -storepass:env CLIPSYNC_STORE_PASSWORD \
    -alias "$KEY_ALIAS" >/dev/null
printf 'PASS: JKS reopened with keytool\n'

(cd "$ANDROID_DIR" && ./gradlew --no-daemon \
    --project-cache-dir "$CLIPSYNC_PROJECT_BUILD_ROOT/gradle-project-cache" \
    clean assembleRelease)
# With the external-storage build redirect active, the app build directory is
# $CLIPSYNC_ANDROID_BUILD_ROOT/app instead of android/app/build.
if [[ -n "${CLIPSYNC_ANDROID_BUILD_ROOT:-}" ]]; then
    SIGNED_APK="$CLIPSYNC_ANDROID_BUILD_ROOT/app/outputs/apk/release/app-release.apk"
else
    SIGNED_APK="$ANDROID_DIR/app/build/outputs/apk/release/app-release.apk"
fi
[[ -f "$SIGNED_APK" ]] || fail "signed release APK missing"
cp "$SIGNED_APK" "$DIST_FILE"
chmod 600 "$DIST_FILE"

"$APKSIGNER_BIN" verify --print-certs "$DIST_FILE"
printf 'PASS: signed APK verified at %s\n' "$DIST_FILE"
