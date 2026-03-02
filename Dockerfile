FROM rust:1.86-bookworm

WORKDIR /app

# Copy lockfile and manifest first for better layer caching.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY scripts ./scripts

RUN ./scripts/install-with-shim.sh
RUN chmod +x ./scripts/integration-linux.sh

CMD ["./scripts/integration-linux.sh"]
