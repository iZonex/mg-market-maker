# ── Build stage ──────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Build release binary.
RUN cargo build --release -p mm-server && \
    strip target/release/mm-server

# ── Runtime stage ───────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

# Non-root user.
RUN useradd -m -s /bin/bash mm
USER mm
WORKDIR /home/mm

COPY --from=builder /app/target/release/mm-server /usr/local/bin/mm-server
COPY config/default.toml config/default.toml

# Data directories.
RUN mkdir -p data/audit data/checkpoint logs

ENV RUST_LOG=info,mm_engine=debug
ENV MM_CONFIG=config/default.toml

EXPOSE 9090

HEALTHCHECK --interval=10s --timeout=3s --retries=3 \
    CMD curl -sf http://localhost:9090/health || exit 1

ENTRYPOINT ["mm-server"]
