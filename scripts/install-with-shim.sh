#!/usr/bin/env bash
set -euo pipefail

# Resolve repository root and run from there so relative paths are stable.
crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$crate_dir"

# Install/compile artifacts:
# - install the CLI into Cargo's bin dir
# - build release shim library used by preload injection
cargo install --path . --force
cargo build --release --lib

# Determine install destination for the shim (same directory as `timewarp`).
bin_dir="${CARGO_HOME:-$HOME/.cargo}/bin"

# Pick the platform-specific shim filename.
case "$(uname -s)" in
  Linux) shim_name="libtimewarp_shim.so" ;;
  Darwin) shim_name="libtimewarp_shim.dylib" ;;
  *)
    echo "unsupported platform for shim install: $(uname -s)" >&2
    exit 1
    ;;
esac

src_shim="target/release/${shim_name}"
dst_shim="${bin_dir}/${shim_name}"

# Validate the release shim exists before copying.
if [[ ! -f "${src_shim}" ]]; then
  echo "expected shim not found: ${src_shim}" >&2
  exit 1
fi

# Copy shim beside installed binary so runtime auto-discovery works.
mkdir -p "${bin_dir}"
cp "${src_shim}" "${dst_shim}"

echo "installed:"
echo "  binary: ${bin_dir}/timewarp"
echo "  shim:   ${dst_shim}"
