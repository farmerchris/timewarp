#!/usr/bin/env bash
set -euo pipefail

# Usage:
#   ./scripts/docker-build-and-compat.sh [image_tag]
# Optional env:
#   SPEED=5 PING_HOST=1.1.1.1

image_tag="${1:-timewarp-linux-test}"
target_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-debug-target"
cargo_registry_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-cargo-registry"
cargo_git_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-cargo-git"
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "[docker] building image: ${image_tag}"
DOCKER_BUILDKIT=1 docker build -t "${image_tag}" "${project_root}"

docker volume create "${target_volume}" >/dev/null
docker volume create "${cargo_registry_volume}" >/dev/null
docker volume create "${cargo_git_volume}" >/dev/null

echo "[docker] running hyperspeed compatibility checks"
docker run --rm \
  -e SPEED="${SPEED:-5}" \
  -e PING_HOST="${PING_HOST:-1.1.1.1}" \
  -v "${target_volume}:/app/target" \
  -v "${cargo_registry_volume}:/usr/local/cargo/registry" \
  -v "${cargo_git_volume}:/usr/local/cargo/git" \
  "${image_tag}" \
  ./scripts/hyperspeed-compat-check.sh
