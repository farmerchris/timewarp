# AGENTS.md

## Project Intent
- `timewarp` is a preload-based time virtualization wrapper.
- Keep behavior explicit: default to wall-clock virtualization with clear warnings for partial coverage.

## Engineering Rules
- Preserve cross-platform guards (`linux`/`macos`) around interposition differences.
- Avoid broad behavior changes without adding probe/test coverage.
- Prefer deterministic parsing and explicit error messages over silent fallbacks.

## Testing Expectations
- Run `cargo build` before finishing edits.
- Run `cargo test` when test-impacting code changes.
- For Linux-time behavior, validate via container flow (`scripts/integration-linux.sh`) when possible.

## Interposition Work
- When adding new hooks, update both:
  - exported symbol wrappers in `src/lib.rs`
  - probe/tests/docs that explain coverage and limitations.
- Keep recursion guards intact (`with_hook_guard`) for all hooked functions.

## UX Conventions
- Keep CLI help text concrete and example-driven.
- Probe output should state both heuristic findings and active-check outcome.
