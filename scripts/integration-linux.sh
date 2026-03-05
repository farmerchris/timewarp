#!/usr/bin/env bash
set -euo pipefail

# Print toolchain versions for reproducibility/debugging.
echo "[integration] rustc: $(rustc --version)"
echo "[integration] cargo: $(cargo --version)"

# Build all local targets used by the test flow.
echo "[integration] building"
cargo build

# Run the Rust test suite.
echo "[integration] running tests"
cargo test -- --nocapture

# Capture a baseline timestamp from the helper binary.
echo "[integration] example: baseline now"
baseline="$(cargo run --quiet --bin now)"
echo "$baseline"

base_epoch="$(printf '%s\n' "$baseline" | awk -F= '/^time=/{print $2; exit}')"
if [[ -z "${base_epoch}" ]]; then
  echo "[integration] failed to parse baseline epoch" >&2
  exit 1
fi

# Run the same helper under a +2h offset and compare epochs.
echo "[integration] example: warped +2h"
warped="$(cargo run --quiet -- --offset +2h target/debug/now)"
echo "$warped"

warped_epoch="$(printf '%s\n' "$warped" | awk -F= '/^time=/{print $2; exit}')"
if [[ -z "${warped_epoch}" ]]; then
  echo "[integration] failed to parse warped epoch" >&2
  exit 1
fi

delta="$((warped_epoch - base_epoch))"
echo "[integration] delta seconds: $delta"

# Allow a small tolerance around exactly +7200 seconds.
if (( delta < 7190 || delta > 7210 )); then
  echo "[integration] expected approximately +7200 seconds shift" >&2
  exit 1
fi

echo "[integration] PASS"
