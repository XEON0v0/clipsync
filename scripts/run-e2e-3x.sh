#!/usr/bin/env bash
# T11 end-to-end gate: run the twenty-nine-scenario e2e suite three times and
# assert exactly 29 tests pass on every pass. Locked Cargo only.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
source "$ROOT_DIR/scripts/require-external-dev.sh"
clipsync_require_external_dev "$ROOT_DIR"
cd "$ROOT_DIR"

for pass in 1 2 3; do
    echo "=== e2e pass ${pass}/3: cargo test --locked -p clipboard-server --test e2e ==="
    status=0
    output="$(cargo test --locked -p clipboard-server --test e2e 2>&1)" || status=$?
    printf '%s\n' "$output"
    if [[ "$status" -ne 0 ]]; then
        echo "FAIL pass ${pass}: cargo test exited ${status}" >&2
        exit 1
    fi
    result_line="$(printf '%s\n' "$output" | grep -E '^test result:' || true)"
    passed="$(printf '%s\n' "$result_line" | sed -nE 's/^test result: ok\. ([0-9]+) passed; 0 failed;.*/\1/p')"
    if [[ "$passed" != "29" ]]; then
        echo "FAIL pass ${pass}: expected 'test result: ok. 29 passed; 0 failed;', got: ${result_line:-<no result line>}" >&2
        exit 1
    fi
    echo "=== pass ${pass}: 29/29 tests passed ==="
done

echo "E2E 3x OK: 29 tests passed on every pass"
