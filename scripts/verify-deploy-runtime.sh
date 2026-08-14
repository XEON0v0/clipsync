#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
source "$repo_root/scripts/require-external-dev.sh"
clipsync_require_external_dev "$repo_root"
deploy_dir="$repo_root/deploy"
base="$deploy_dir/docker-compose.yml"
local_override="$deploy_dir/docker-compose.local.yml"
domain="validate.example"

command -v docker >/dev/null 2>&1 || {
    echo "FAIL: Docker CLI is required" >&2
    exit 1
}
docker info >/dev/null 2>&1 || {
    echo "FAIL: Docker daemon is unavailable" >&2
    exit 1
}

compose_version="$(docker compose version --short)"
ruby -e 'require "rubygems"; abort "Docker Compose 2.24.4+ required" if Gem::Version.new(ARGV[0]) < Gem::Version.new("2.24.4")' "$compose_version"

DOMAIN="$domain" docker compose -f "$base" config --quiet
resolved="$(DOMAIN="$domain" docker compose -f "$base" -f "$local_override" config)"
printf '%s\n' "$resolved" | ruby -ryaml -e '
  c = YAML.load(STDIN.read)
  ports = c.dig("services", "caddy", "ports")
  abort "local override must resolve to one port" unless ports&.length == 1
  p = ports.fetch(0)
  abort "local port must bind loopback 8443->443" unless p["host_ip"] == "127.0.0.1" && p["published"].to_s == "8443" && p["target"].to_s == "443"
  abort "server must publish no ports" if c.dig("services", "server").key?("ports")
'

builder_image="$(LC_ALL=C sed -nE 's/^FROM[[:space:]]+([^[:space:]]+).*/\1/p' "$deploy_dir/Dockerfile" | sed -n '1p')"
runtime_image="$(LC_ALL=C sed -nE 's/^FROM[[:space:]]+([^[:space:]]+).*/\1/p' "$deploy_dir/Dockerfile" | sed -n '2p')"
caddy_image="$(LC_ALL=C sed -nE 's/^[[:space:]]+image:[[:space:]]+(caddy:[^[:space:]]+)/\1/p' "$base")"

for image in "$builder_image" "$runtime_image" "$caddy_image"; do
    docker pull "$image"
    docker image inspect "$image" --format '{{json .RepoDigests}}'
done

docker run --rm -e DOMAIN="$domain" -v "$deploy_dir/Caddyfile:/etc/caddy/Caddyfile:ro" "$caddy_image" caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
docker run --rm -v "$deploy_dir/Caddyfile.local:/etc/caddy/Caddyfile:ro" "$caddy_image" caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile

project="clipsync_t12_$$"
cleanup() {
    DOMAIN="$domain" docker compose -p "$project" -f "$base" -f "$local_override" down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT

DOMAIN="$domain" docker compose -p "$project" -f "$base" -f "$local_override" up --build --detach --wait
server_id="$(DOMAIN="$domain" docker compose -p "$project" -f "$base" -f "$local_override" ps -q server)"
caddy_id="$(DOMAIN="$domain" docker compose -p "$project" -f "$base" -f "$local_override" ps -q caddy)"

[[ "$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$server_id")" == "172.30.0.3" ]]
[[ "$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$caddy_id")" == "172.30.0.2" ]]
[[ "$(docker inspect -f '{{.State.Health.Status}}' "$server_id")" == "healthy" ]]
[[ "$(docker inspect -f '{{range .Config.Env}}{{println .}}{{end}}' "$server_id" | awk -F= '$1=="TRUSTED_PROXY" {print $2}')" == "172.30.0.2/32" ]]

echo "PASS: Docker images, Caddy validation, Compose resolution, fixed IPs, /32 trust, and health"
