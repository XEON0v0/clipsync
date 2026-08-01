#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RULE="$ROOT_DIR/scripts/audit/server-no-content-crypto.yml"
FIXTURES="$ROOT_DIR/scripts/audit/fixtures/server-no-content-crypto"
SG="${SG:-$(command -v sg || command -v ast-grep || true)}"

if [[ -z "$SG" ]]; then
    printf 'FAIL: ast-grep executable (sg or ast-grep) is required\n' >&2
    exit 1
fi

scan_count() {
    local target="$1"
    local output status
    set +e
    output="$($SG scan --json=stream --rule "$RULE" "$target" 2>&1)"
    status=$?
    set -e
    # ast-grep versions differ on whether an empty scan exits 0 or 1. Both are
    # normalized here; parser/configuration failures use other statuses.
    if (( status > 1 )); then
        printf '%s\n' "$output" >&2
        return "$status"
    fi
    if [[ -z "$output" ]]; then
        printf '0\n'
    else
        printf '%s\n' "$output" | awk 'NF { count += 1 } END { print count + 0 }'
    fi
}

assert_clean() {
    local target="$1"
    local count
    count="$(scan_count "$target")"
    [[ "$count" == 0 ]] || {
        printf 'FAIL: expected no forbidden crypto matches in %s, got %s\n' "$target" "$count" >&2
        exit 1
    }
    printf 'PASS: no forbidden crypto matches in %s\n' "$target"
}

assert_detected() {
    local target="$1"
    local count
    count="$(scan_count "$target")"
    (( count >= 1 )) || {
        printf 'FAIL: expected at least one forbidden crypto match in %s\n' "$target" >&2
        exit 1
    }
    printf 'PASS: detected %s forbidden crypto match(es) in %s\n' "$count" "$target"
}

assert_clean "$ROOT_DIR/crates/server/src"
assert_clean "$FIXTURES/clean.rs"
assert_detected "$FIXTURES/forbidden-import.rs"
assert_detected "$FIXTURES/forbidden-path.rs"
assert_detected "$FIXTURES/forbidden-call.rs"
