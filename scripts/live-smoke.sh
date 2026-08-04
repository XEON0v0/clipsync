#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BASE_COMPOSE="$ROOT_DIR/deploy/docker-compose.yml"
LOCAL_COMPOSE="$ROOT_DIR/deploy/docker-compose.local.yml"
RUNBOOK="$ROOT_DIR/deploy/RUNBOOK.md"
MACOS_APP="$ROOT_DIR/dist/ClipboardSync.app"
ANDROID_APK="$ROOT_DIR/dist/clipboard-sync.apk"
LOCAL_URL="wss://localhost:8443/ws"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/clipsync-live-smoke.XXXXXX")"
ENV_FILE="$TMP_ROOT/compose.env"
CA_PEM="$TMP_ROOT/caddy-root.pem"
CA_DER="$TMP_ROOT/caddy-root.der"
SERVER_LOG="$TMP_ROOT/server.log"
PROJECT="clipsync_live_${$}_${RANDOM}"

printf 'DOMAIN=localhost\n' > "$ENV_FILE"
compose=(
    docker compose
    --env-file "$ENV_FILE"
    -p "$PROJECT"
    -f "$BASE_COMPOSE"
    -f "$LOCAL_COMPOSE"
)

cleanup() {
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$TMP_ROOT"
}
trap cleanup EXIT INT TERM

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    exit 1
}

pass() {
    printf 'PASS: %s\n' "$*"
}

command -v docker >/dev/null 2>&1 || fail "Docker CLI is required"
docker info >/dev/null 2>&1 || fail "Docker daemon is unavailable"
command -v openssl >/dev/null 2>&1 || fail "OpenSSL is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"
command -v shasum >/dev/null 2>&1 || fail "shasum is required"
command -v codesign >/dev/null 2>&1 || fail "codesign is required"
CARGO_BIN="$(command -v cargo || true)"
if [[ -z "$CARGO_BIN" && -x "$HOME/.cargo/bin/cargo" ]]; then
    CARGO_BIN="$HOME/.cargo/bin/cargo"
fi
[[ -n "$CARGO_BIN" ]] || fail "Cargo is required"
CARGO_BIN_DIR="$(dirname "$CARGO_BIN")"

resolved_json="$("${compose[@]}" config --format json)" \
    || fail "base and local Compose configuration did not resolve"
printf '%s\n' "$resolved_json" | jq -e '
    (.services.server | has("ports") | not)
    and (.services.caddy.ports | length == 1)
    and (.services.caddy.ports[0].host_ip == "127.0.0.1")
    and (.services.caddy.ports[0].published == "8443")
    and (.services.caddy.ports[0].target == 443)
' >/dev/null || fail "resolved Compose ports must expose only 127.0.0.1:8443 via Caddy"
pass "resolved Compose publishes Caddy 8443 only; server 8080 remains internal"

"${compose[@]}" up --build --detach --wait --wait-timeout 240 \
    || fail "local Compose stack failed to become healthy"
pass "base and local Compose stack is healthy"

caddy_id="$("${compose[@]}" ps -q caddy)"
server_id="$("${compose[@]}" ps -q server)"
[[ -n "$caddy_id" && -n "$server_id" ]] || fail "Compose container IDs are missing"
expected_effective_ip="$(docker inspect -f \
    '{{range .NetworkSettings.Networks}}{{.Gateway}}{{end}}' "$caddy_id")"
[[ -n "$expected_effective_ip" ]] \
    || fail "could not resolve the Docker bridge address seen by Caddy"

docker cp "$caddy_id:/data/caddy/pki/authorities/local/root.crt" "$CA_PEM" >/dev/null \
    || fail "could not export Caddy local root CA"
openssl x509 -in "$CA_PEM" -outform DER -out "$CA_DER" \
    || fail "could not convert Caddy root CA to DER"
openssl x509 -in "$CA_PEM" -noout -subject >/dev/null \
    || fail "exported Caddy root CA is invalid"
pass "Caddy local root CA exported from its data volume"

if [[ "${CLIPSYNC_SMOKE_STOP_SERVER_BEFORE_CLIENT:-0}" == "1" ]]; then
    "${compose[@]}" stop server >/dev/null || fail "could not stop server for negative test"
    pass "server stopped for negative-path assertion"
fi

if ! (
    cd "$ROOT_DIR"
    PATH="$CARGO_BIN_DIR:$PATH" "$CARGO_BIN" run --locked -p clipboard-server \
        --example sim_client -- --url "$LOCAL_URL" --ca "$CA_DER"
); then
    fail "WSS simulated client failed at $LOCAL_URL"
fi
pass "WSS pairing, payload, bootstrap, and forged-XFF simulation"

"${compose[@]}" logs --no-color server > "$SERVER_LOG" \
    || fail "could not read server structured logs"
grep -Eq 'event=rate_limited kind=join effective_ip=[^ ]+ connection_id=[0-9]+' "$SERVER_LOG" \
    || fail "server log has no structured join rate-limit event"
if grep -Fq '203.0.113.9' "$SERVER_LOG"; then
    fail "server log trusted the forged X-Forwarded-For address"
fi
effective_ip="$(sed -nE 's/.*event=rate_limited kind=join effective_ip=([^ ]+).*/\1/p' "$SERVER_LOG" | head -1)"
[[ "$effective_ip" == "$expected_effective_ip" ]] \
    || fail "rate-limit effective_ip=$effective_ip did not match Caddy-seen client $expected_effective_ip"
pass "rate-limit log records Caddy-seen effective_ip=$effective_ip and excludes forged XFF"

[[ -d "$MACOS_APP" ]] || fail "macOS app bundle is missing: $MACOS_APP"
/usr/bin/codesign --verify --strict "$MACOS_APP" \
    || fail "macOS app signature verification failed"
codesign_output="$(/usr/bin/codesign --display --verbose=4 "$MACOS_APP" 2>&1)" \
    || fail "could not inspect macOS app signature"
grep -Fq 'Signature=adhoc' <<< "$codesign_output" \
    || fail "macOS app is not ad-hoc signed"
pass "macOS app ad-hoc signature verified"

SDK_ROOT="${ANDROID_SDK_ROOT:-${ANDROID_HOME:-$HOME/Library/Android/sdk}}"
APKSIGNER_BIN="$(command -v apksigner || true)"
if [[ -z "$APKSIGNER_BIN" ]]; then
    for candidate in \
        "$SDK_ROOT/build-tools/34.0.0/apksigner" \
        "$SDK_ROOT/build-tools/35.0.0/apksigner" \
        "$SDK_ROOT/build-tools/36.1.0/apksigner"; do
        if [[ -x "$candidate" ]]; then
            APKSIGNER_BIN="$candidate"
            break
        fi
    done
fi
[[ -n "$APKSIGNER_BIN" ]] || fail "apksigner is required"
[[ -f "$ANDROID_APK" ]] || fail "Android APK is missing: $ANDROID_APK"
"$APKSIGNER_BIN" verify --print-certs "$ANDROID_APK" >/dev/null \
    || fail "Android APK signature verification failed"
pass "Android APK signature verified"

grep -Fq 'Do not expose TCP 8080.' "$RUNBOOK" \
    || fail "RUNBOOK does not prohibit public TCP 8080"
grep -Fq 'actual WebSocket TLS path' "$RUNBOOK" \
    || fail "RUNBOOK does not require a real WebSocket TLS check"
grep -Fq 'scripts/live-smoke.sh' "$RUNBOOK" \
    || fail "RUNBOOK does not name the live smoke script"
grep -Fq 'ICP filing' "$RUNBOOK" \
    || fail "RUNBOOK is missing the mainland China ICP note"
pass "RUNBOOK documents ICP, WSS verification, and the no-8080 boundary"

macos_digest="$(shasum -a 256 "$MACOS_APP/Contents/MacOS/ClipSyncMacOS" | awk '{print $1}')"
android_digest="$(shasum -a 256 "$ANDROID_APK" | awk '{print $1}')"
[[ "$macos_digest" =~ ^[0-9a-f]{64}$ ]] || fail "invalid macOS artifact digest"
[[ "$android_digest" =~ ^[0-9a-f]{64}$ ]] || fail "invalid Android artifact digest"
pass "artifact SHA-256 macOS=$macos_digest android=$android_digest"

pass "live container smoke completed"
