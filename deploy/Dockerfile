FROM rust:1.90.0-bookworm@sha256:3914072ca0c3b8aad871db9169a651ccfce30cf58303e5d6f2db16d1d8a7e58f AS builder

WORKDIR /workspace
ARG CARGO_REGISTRY_CONFIG=deploy/cargo-config.crates-io.toml
ENV RUSTUP_TOOLCHAIN=1.90.0
COPY ${CARGO_REGISTRY_CONFIG} /usr/local/cargo/config.toml
COPY . .
RUN cargo build --locked --release -p clipboard-server

FROM debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

RUN groupadd --gid 10001 clipsync \
    && useradd --uid 10001 --gid clipsync --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin clipsync \
    && install -d -o clipsync -g clipsync -m 0700 /data
COPY --from=builder --chown=clipsync:clipsync /workspace/target/release/clipsync-server /usr/local/bin/clipsync-server

ENV BIND_ADDR=0.0.0.0:8080 \
    DATA_DIR=/data \
    MAILBOX_DIR=/data/mailboxes
USER 10001:10001
EXPOSE 8080
VOLUME ["/data"]
HEALTHCHECK --interval=15s --timeout=5s --start-period=5s --retries=3 CMD ["/usr/local/bin/clipsync-server", "--healthcheck"]
ENTRYPOINT ["/usr/local/bin/clipsync-server"]
