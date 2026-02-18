# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder
WORKDIR /workspace

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

RUN cargo build --release --locked --package waddle-server --bin waddle-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /var/lib/waddle --shell /usr/sbin/nologin waddle

WORKDIR /app

COPY --from=builder /workspace/target/release/waddle-server /usr/local/bin/waddle-server
COPY certs ./certs

ENV WADDLE_MODE=standalone \
    WADDLE_BASE_URL=http://127.0.0.1:3000 \
    WADDLE_DB_PATH=/var/lib/waddle/waddle.db \
    WADDLE_XMPP_TLS_CERT=/app/certs/server.crt \
    WADDLE_XMPP_TLS_KEY=/app/certs/server.key \
    RUST_LOG=info

EXPOSE 3000 5222 5269

VOLUME ["/var/lib/waddle"]

USER waddle:waddle

ENTRYPOINT ["/usr/local/bin/waddle-server"]
