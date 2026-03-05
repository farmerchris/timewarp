# Compatibility Utilities

Use these utilities to validate `timewarp --hyperspeed` behavior across common timing paths.

## Core Checks
- `sleep` (coreutils): baseline sleep-hook validation (`sleep/usleep/nanosleep` path).
- `ping` (iputils): network pacing loop (`poll` + monotonic clock scheduling).
- `timeout` (coreutils): alarm/timer behavior (`setitimer`/signal timeout path).

## Additional Useful Checks
- `bash` builtin `read -t`: timeout wait behavior in shell internals.
- `curl --max-time`: application-level timeout handling.
- `openssl s_client` with timeout wrappers: TLS/network timeout interactions.

## Notes
- Success for one utility does not imply all binaries are compatible.
- Some tools use kernel/vDSO/direct syscalls and can bypass user-space interposition.
- Prefer running these checks in Linux for strongest coverage.
