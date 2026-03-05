#!/usr/bin/env bash
set -euo pipefail

# Optional first arg overrides the default image tag.
image_tag="${1:-timewarp-linux-test}"
target_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-debug-target"
cargo_registry_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-cargo-registry"
cargo_git_volume="${image_tag//[^a-zA-Z0-9_.-]/_}-cargo-git"
# Resolve repo root regardless of current working directory.
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Build the integration image from the project root.
echo "[docker] building image: ${image_tag}"
DOCKER_BUILDKIT=1 docker build -t "${image_tag}" "${project_root}"

# Run the container's default integration command.
echo "[docker] running tests in container: ${image_tag}"
docker volume create "${target_volume}" >/dev/null
docker volume create "${cargo_registry_volume}" >/dev/null
docker volume create "${cargo_git_volume}" >/dev/null
docker run --rm \
  -v "${target_volume}:/app/target" \
  -v "${cargo_registry_volume}:/usr/local/cargo/registry" \
  -v "${cargo_git_volume}:/usr/local/cargo/git" \
  "${image_tag}" \
  ./scripts/integration-linux.sh
