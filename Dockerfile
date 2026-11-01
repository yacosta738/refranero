# syntax=docker/dockerfile:1

# ---- Build stage -----------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /build

# rusqlite's "bundled" feature compiles SQLite from C sources.
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY data ./data

RUN cargo build --release --locked \
    && strip target/release/refranero

# ---- Runtime stage ----------------------------------------------------------
FROM debian:bookworm-slim AS runtime

RUN groupadd --system refranero \
    && useradd --system --gid refranero --create-home --home-dir /home/refranero refranero \
    && mkdir -p /home/refranero/.local/share/refranero \
    && chown -R refranero:refranero /home/refranero

COPY --from=builder /build/target/release/refranero /usr/local/bin/refranero
COPY docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh

RUN chmod 755 /usr/local/bin/docker-entrypoint.sh

USER refranero
WORKDIR /home/refranero
VOLUME ["/home/refranero/.local/share"]

ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["--help"]
