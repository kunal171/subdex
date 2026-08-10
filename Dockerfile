# syntax=docker/dockerfile:1

# ── Build stage ───────────────────────────────────────────────────────────────
# Pinned to the same toolchain CI builds with (1.96.0). Note the workspace's
# declared rust-version (1.85) is below what the dependency tree actually needs —
# sqlx 0.9 requires rustc >= 1.94 — so we track CI, not the manifest floor.
# The build needs no database: subdex uses sqlx in runtime-query mode (no
# `query!` compile-time macros, no .sqlx cache), so nothing here connects to PG.
FROM rust:1.96.0-bookworm AS builder

# Which workspace binary to ship. Defaults to the `transfers` example indexer;
# override with `--build-arg BIN=multi-pallet` (and PACKAGE) to ship another.
ARG BIN=transfers
ARG PACKAGE=subdex-example-transfers

WORKDIR /build

# Cache dependency compilation separately from source: copy only the manifests
# first, build a stub, then copy real sources. A source-only change then reuses
# the dependency layer. cargo has no first-class "deps only", so we vendor the
# manifests and let the registry/target caches (mounted below) do the heavy work.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY examples ./examples

# BuildKit cache mounts keep the cargo registry and target dir warm across
# builds without baking them into the image.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --locked -p ${PACKAGE} --bin ${BIN} \
    && cp target/release/${BIN} /build/indexer

# ── Runtime stage ─────────────────────────────────────────────────────────────
# Debian slim (not scratch/alpine): rustls needs the system CA bundle to verify
# the chain's WSS endpoint, and glibc avoids musl surprises with tokio/subxt.
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Run as a non-root user.
RUN useradd --system --uid 10001 --home-dir /app --shell /usr/sbin/nologin subdex
WORKDIR /app

COPY --from=builder /build/indexer /usr/local/bin/indexer

USER subdex

# GraphQL API (example default 4350). The indexer also needs WS_URL + DATABASE_URL
# at runtime — see docker-compose.yml or pass them with `-e`.
EXPOSE 4350
ENV RUST_LOG=info

ENTRYPOINT ["/usr/local/bin/indexer"]
