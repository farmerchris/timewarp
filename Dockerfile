# syntax=docker/dockerfile:1.7
FROM rust:1-slim-bookworm

WORKDIR /app

RUN apt-get update && \
    apt-get install -y --no-install-recommends file && \
    rm -rf /var/lib/apt/lists/*

# Copy lockfile and manifest first for better layer caching.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY scripts ./scripts

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    ./scripts/install-with-shim.sh 
