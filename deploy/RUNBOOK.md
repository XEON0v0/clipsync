# ClipSync Relay Runbook

## Host and DNS

Use a current Debian or Ubuntu ECS host with Docker Engine and Docker Compose v2.24.4 or newer. Create an `A` record for the relay hostname pointing to the ECS public IPv4 address. Add an `AAAA` record only when the host has working public IPv6.

In the Alibaba Cloud security group, allow inbound TCP 80 and TCP 443 from the internet. UDP 443 is optional for HTTP/3. Do not expose TCP 8080. Restrict SSH to an administrator IP range.

Mainland China deployments may require ICP filing before a public domain can serve traffic. Confirm the current Alibaba Cloud and local regulatory requirements before switching DNS.

## Deploy

Create the environment file without committing it:

```sh
cd deploy
cp .env.example .env
chmod 600 .env
```

Set `DOMAIN` to the real relay hostname, then validate and start:

```sh
../scripts/test-deploy.sh
REQUIRE_DOCKER=1 ../scripts/verify-deploy-runtime.sh
docker compose up --build -d
docker compose ps
```

Caddy obtains and renews the public certificate automatically. The relay container has no published port; only Caddy at `172.30.0.2` can reach `server:8080`, and the relay trusts exactly `172.30.0.2/32` for `X-Forwarded-For`.

The server build uses the official crates.io sparse index by default. If that index is unreachable from mainland China, explicitly set `CARGO_REGISTRY_CONFIG=deploy/cargo-config.rsproxy.toml` for the `docker compose build` command. `Cargo.lock` and registry checksums fix every dependency with either source.

## Verify

Confirm HTTPS health from outside the ECS host:

```sh
curl --fail --show-error --silent "https://${DOMAIN}/healthz"
```

The response must be `ok`. A `503 degraded` response means mailbox persistence failed; inspect disk ownership, free space, and server logs before accepting new traffic.

Exercise the actual WebSocket TLS path with a WebSocket client. A successful upgrade followed by a valid protocol `hello` must return `hello_ok`; an HTTP-only health check is not sufficient for release verification. The repository's `scripts/live-smoke.sh` performs this full WSS exchange once T17 is present.

```sh
docker compose logs --tail=100 server caddy
docker compose exec -T server /usr/local/bin/clipsync-server --healthcheck
```

## Operations

Restart or upgrade only after validating the resolved configuration:

```sh
docker compose config --quiet
docker compose pull caddy
docker compose up --build -d
```

Prune unactivated rooms older than 24 hours without stopping the live service:

```sh
docker compose run --rm server --prune-unactivated --older-than 24h
```

## Backup and Restore

Back up both the relay data and Caddy certificate data. The server volume contains `rooms.json` and opaque encrypted mailbox snapshots; the Caddy volume contains certificate private keys. Store archives encrypted and access-controlled.

```sh
docker run --rm -v clipsync_server_data:/source:ro -v "$PWD/backups:/backup" debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 tar -C /source -czf /backup/server-data.tgz .
docker run --rm -v clipsync_caddy_data:/source:ro -v "$PWD/backups:/backup" debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818 tar -C /source -czf /backup/caddy-data.tgz .
```

To restore, stop the stack, restore each archive into its matching empty named volume, verify ownership, and start the stack. Never edit `rooms.json` or mailbox files by hand.
