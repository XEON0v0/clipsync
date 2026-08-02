#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
deploy_dir="$repo_root/deploy"
fixture_dir="$repo_root/scripts/fixtures/deploy"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

validate_prod_caddy() {
    local file="$1"
    ! grep -Fq '${DOMAIN}' "$file" \
        && ! grep -Fq ';' "$file" \
        && grep -Eq '^\{\$DOMAIN\} \{$' "$file" \
        && grep -Eq '^[[:space:]]+reverse_proxy server:8080$' "$file" \
        && [[ "$(grep -cve '^[[:space:]]*$' "$file")" -eq 3 ]]
}

validate_local_caddy() {
    local file="$1"
    ! grep -Fq ';' "$file" \
        && grep -Eq '^localhost \{$' "$file" \
        && grep -Eq '^[[:space:]]+tls internal$' "$file" \
        && grep -Eq '^[[:space:]]+reverse_proxy server:8080$' "$file" \
        && [[ "$(grep -cve '^[[:space:]]*$' "$file")" -eq 4 ]]
}

validate_local_override() {
    local file="$1"
    [[ "$(grep -Ec '^[[:space:]]+(ports|volumes): !override$' "$file")" -eq 2 ]] \
        && grep -Fq '"127.0.0.1:8443:443"' "$file" \
        && ! grep -Fq '"80:80"' "$file" \
        && ! grep -Fq '"443:443"' "$file"
}

validate_trusted_proxy() {
    local file="$1"
    grep -Eq '^[[:space:]]+TRUSTED_PROXY: 172\.30\.0\.2/32$' "$file"
}

expect_rejected() {
    local label="$1"
    shift
    if "$@"; then
        fail "negative fixture was accepted: $label"
    fi
    echo "PASS negative fixture rejected: $label"
}

from_refs="$(LC_ALL=C sed -nE 's/^FROM[[:space:]]+([^[:space:]]+).*/\1/p' "$deploy_dir/Dockerfile")"
[[ "$(printf '%s\n' "$from_refs" | grep -c .)" -eq 2 ]] || fail "Dockerfile must have two FROM images"
while IFS= read -r image; do
    [[ "$image" =~ @sha256:[0-9a-f]{64}$ ]] || fail "unpinned Dockerfile image: $image"
done <<< "$from_refs"

caddy_image="$(LC_ALL=C sed -nE 's/^[[:space:]]+image:[[:space:]]+(caddy:[^[:space:]]+)/\1/p' "$deploy_dir/docker-compose.yml")"
[[ "$caddy_image" =~ ^caddy:[^@]+@sha256:[0-9a-f]{64}$ ]] || fail "Caddy image is not digest-pinned"
[[ "$(grep -Ec '^[[:space:]]*RUN cargo ' "$deploy_dir/Dockerfile")" -eq 1 ]] || fail "Dockerfile must have one Cargo build command"
grep -Fqx 'RUN cargo build --locked --release -p clipboard-server' "$deploy_dir/Dockerfile" || fail "Docker build must use locked server-only command"
grep -Fqx 'ARG CARGO_REGISTRY_CONFIG=deploy/cargo-config.crates-io.toml' "$deploy_dir/Dockerfile" || fail "Docker build must default to the official crates.io index"
grep -Fq 'ENV RUSTUP_TOOLCHAIN=1.90.0' "$deploy_dir/Dockerfile" || fail "Docker build must use the image's pinned installed toolchain"
grep -Fqx 'COPY ${CARGO_REGISTRY_CONFIG} /usr/local/cargo/config.toml' "$deploy_dir/Dockerfile" || fail "Docker build must install the selected Cargo source config"
grep -Fqx 'replace-with = "rsproxy"' "$deploy_dir/cargo-config.rsproxy.toml" || fail "rsproxy config must replace crates.io"
grep -Fqx 'registry = "sparse+https://rsproxy.cn/index/"' "$deploy_dir/cargo-config.rsproxy.toml" || fail "rsproxy config must use its sparse index"
grep -Fqx 'multiplexing = false' "$deploy_dir/cargo-config.crates-io.toml" || fail "official Cargo config must tolerate proxies without HTTP/2 multiplexing"
grep -Fq 'USER 10001:10001' "$deploy_dir/Dockerfile" || fail "runtime image must be non-root"
grep -Fq 'CMD ["/usr/local/bin/clipsync-server", "--healthcheck"]' "$deploy_dir/Dockerfile" || fail "binary healthcheck missing"
! grep -Eq 'docker run.*debian:bookworm-slim([[:space:]]|$)' "$deploy_dir/RUNBOOK.md" \
    || fail "RUNBOOK backup image must be digest-pinned"
[[ "$(grep -Fc 'debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818' "$deploy_dir/RUNBOOK.md")" -eq 2 ]] \
    || fail "RUNBOOK must use the pinned runtime image for both backup commands"

validate_prod_caddy "$deploy_dir/Caddyfile" || fail "production Caddyfile structure"
validate_local_caddy "$deploy_dir/Caddyfile.local" || fail "local Caddyfile structure"
validate_local_override "$deploy_dir/docker-compose.local.yml" || fail "local Compose must replace ports and volumes"
validate_trusted_proxy "$deploy_dir/docker-compose.yml" || fail "trusted proxy must be Caddy /32"
grep -Fq 'CARGO_REGISTRY_CONFIG: ${CARGO_REGISTRY_CONFIG:-deploy/cargo-config.crates-io.toml}' "$deploy_dir/docker-compose.yml" \
    || fail "Compose must default to the official overridable Cargo source config"

ruby -ryaml -e '
  c = YAML.load_file(ARGV.fetch(0))
  abort "server publishes ports" if c.dig("services", "server").key?("ports")
  abort "bad subnet" unless c.dig("networks", "clipsync", "ipam", "config", 0, "subnet") == "172.30.0.0/24"
  abort "bad caddy IP" unless c.dig("services", "caddy", "networks", "clipsync", "ipv4_address") == "172.30.0.2"
  abort "bad server IP" unless c.dig("services", "server", "networks", "clipsync", "ipv4_address") == "172.30.0.3"
' "$deploy_dir/docker-compose.yml"

expect_rejected 'Caddy ${DOMAIN}' validate_prod_caddy "$fixture_dir/domain-substitution.Caddyfile"
expect_rejected 'single-line Caddyfile' validate_prod_caddy "$fixture_dir/single-line.Caddyfile"
expect_rejected 'local ports without !override' validate_local_override "$fixture_dir/local-without-override.yml"
expect_rejected 'invalid TRUSTED_PROXY CIDR' validate_trusted_proxy "$fixture_dir/invalid-trusted-proxy.yml"

echo "PASS: deployment structure and negative fixtures"
