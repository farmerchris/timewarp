#!/usr/bin/env bash
set -euo pipefail

# Quick compatibility benchmark for --hyperspeed.
# Measures real elapsed time and compares against expected accelerated duration.

SPEED="${SPEED:-5}"
PING_HOST="${PING_HOST:-1.1.1.1}"
TIMEWARP_BIN="${TIMEWARP_BIN:-/usr/local/cargo/bin/timewarp}"

if [[ ! -x "${TIMEWARP_BIN}" ]]; then
  echo "[compat] timewarp binary not found at ${TIMEWARP_BIN}; building debug binary fallback"
  cargo build >/dev/null
  TIMEWARP_BIN="./target/debug/timewarp"
fi

if [[ ! -x "${TIMEWARP_BIN}" ]]; then
  echo "[compat] cannot execute timewarp at ${TIMEWARP_BIN}" >&2
  exit 1
fi

if [[ "${SPEED}" == "0" ]]; then
  echo "[compat] SPEED must be > 0 for duration comparisons" >&2
  exit 1
fi

pass=0
fail=0
skip=0

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

measure_ms() {
  local start_ns end_ns
  start_ns="$(date +%s%N)"
  "$@" >/dev/null 2>&1
  end_ns="$(date +%s%N)"
  echo $(((end_ns - start_ns) / 1000000))
}

threshold_ms() {
  local virtual_s="$1"
  local slack_ms="$2"
  # threshold = (virtual_seconds / speed) + slack
  awk -v v="${virtual_s}" -v s="${SPEED}" -v slack="${slack_ms}" 'BEGIN {
    printf "%.0f\n", ((v / s) * 1000.0) + slack
  }'
}

record_result() {
  local name="$1" elapsed_ms="$2" max_ms="$3"
  if (( elapsed_ms <= max_ms )); then
    echo "[PASS] ${name}: elapsed=${elapsed_ms}ms expected<=${max_ms}ms"
    pass=$((pass + 1))
  else
    echo "[FAIL] ${name}: elapsed=${elapsed_ms}ms expected<=${max_ms}ms"
    fail=$((fail + 1))
  fi
}

run_sleep_case() {
  if ! have_cmd sleep; then
    echo "[SKIP] sleep: command not found"
    skip=$((skip + 1))
    return
  fi
  local virtual_s=5
  local max_ms
  max_ms="$(threshold_ms "${virtual_s}" 2500)"
  local elapsed_ms
  elapsed_ms="$(measure_ms "${TIMEWARP_BIN}" --hyperspeed "${SPEED}" sleep "${virtual_s}")"
  record_result "sleep ${virtual_s}s @${SPEED}x" "${elapsed_ms}" "${max_ms}"
}

run_timeout_case() {
  if ! have_cmd timeout; then
    echo "[SKIP] timeout: command not found"
    skip=$((skip + 1))
    return
  fi
  if ! have_cmd sleep; then
    echo "[SKIP] timeout: sleep not found"
    skip=$((skip + 1))
    return
  fi
  local virtual_s=8
  local max_ms
  max_ms="$(threshold_ms "${virtual_s}" 3000)"
  local elapsed_ms
  # timeout should terminate the wrapped sleep around virtual_s virtual seconds.
  set +e
  elapsed_ms="$(measure_ms "${TIMEWARP_BIN}" --hyperspeed "${SPEED}" timeout "${virtual_s}" sleep 120)"
  local status=$?
  set -e
  if (( status != 0 && status != 124 )); then
    echo "[FAIL] timeout ${virtual_s}s @${SPEED}x: command failed with status=${status}"
    fail=$((fail + 1))
    return
  fi
  record_result "timeout ${virtual_s}s @${SPEED}x" "${elapsed_ms}" "${max_ms}"
}

run_ping_case() {
  if [[ "$(uname -s)" != "Linux" ]]; then
    echo "[SKIP] ping: this check is defined for Linux ping (-w deadline)"
    skip=$((skip + 1))
    return
  fi
  if ! have_cmd ping; then
    echo "[SKIP] ping: command not found"
    skip=$((skip + 1))
    return
  fi
  local virtual_s=10
  local max_ms
  max_ms="$(threshold_ms "${virtual_s}" 5000)"
  local elapsed_ms

  # Send 10 packets at 1s interval; hyperspeed should compress real elapsed.
  set +e
  elapsed_ms="$(measure_ms "${TIMEWARP_BIN}" --hyperspeed "${SPEED}" ping -i 1 -c 10 "${PING_HOST}")"
  local status=$?
  set -e
  # ping may return 1 in some network conditions; still record timing result.
  if (( status != 0 && status != 1 )); then
    echo "[FAIL] ping -c 10 ${PING_HOST} @${SPEED}x: command failed with status=${status}"
    fail=$((fail + 1))
    return
  fi
  if (( status == 1 )); then
    echo "[compat] ping exited with status=1; evaluating elapsed-time check anyway"
  fi
  record_result "ping -c 10 ${PING_HOST} @${SPEED}x" "${elapsed_ms}" "${max_ms}"
}

echo "[compat] using timewarp=${TIMEWARP_BIN} speed=${SPEED} ping_host=${PING_HOST}"

run_sleep_case
run_timeout_case
run_ping_case

echo "[compat] summary: pass=${pass} fail=${fail} skip=${skip}"
if (( fail > 0 )); then
  exit 1
fi
