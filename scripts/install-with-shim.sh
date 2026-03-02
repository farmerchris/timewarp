#!/usr/bin/env bash
set -euo pipefail

# Install the CLI binary via cargo, then place the preload shim next to it.
crate_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$crate_dir"

cargo install --path . --force
cargo build --release --lib

bin_dir="${CARGO_HOME:-$HOME/.cargo}/bin"

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

if [[ ! -f "${src_shim}" ]]; then
  echo "expected shim not found: ${src_shim}" >&2
  exit 1
fi

mkdir -p "${bin_dir}"
cp "${src_shim}" "${dst_shim}"

echo "installed:"
echo "  binary: ${bin_dir}/timewarp"
echo "  shim:   ${dst_shim}"
