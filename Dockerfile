# syntax=docker/dockerfile:1.7
FROM rust:1.93-slim-bookworm

WORKDIR /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends file iputils-ping

# Copy lockfile and manifest first for better layer caching.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY scripts ./scripts

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    ./scripts/install-with-shim.sh 

RUN chmod +x ./scripts/integration-linux.sh ./scripts/hyperspeed-compat-check.sh
