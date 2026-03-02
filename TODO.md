# TODO

## Timeout/Sleep Coverage
- Add `clock_nanosleep` interposition and speed scaling for requested timeout values.
- Add `select`/`pselect` timeout scaling so interval waits accelerate under `--hyperspeed`.
- Add `poll`/`ppoll` timeout scaling so loop pacing in tools like `ping` can accelerate.
- Add Linux aliases (where relevant) for timeout functions that may be called via glibc internal symbols.

## Verification
- Add focused integration fixtures that use each timeout family (`nanosleep`, `clock_nanosleep`, `select`, `poll`) and assert expected runtime deltas with `--hyperspeed`.
- Add a Linux-only integration example for `ping` pacing (best-effort; skip when CAP_NET_RAW is unavailable).

## UX
- Add a `--debug-hooks` mode that logs which interposed functions are hit, to explain why speed changes may not apply for a target.
- Print a probe warning when `--speed`/`--hyperspeed` is set but active probe cannot detect wall-clock drift.
