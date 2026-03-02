# timewarp

`timewarp` runs a command under virtual time using preload-based interposition.

## Scope
- Per-process wall-clock virtualization (not system clock changes).
- Best-effort support on Linux/macOS for dynamically linked binaries.
- Not guaranteed for static binaries, setuid binaries, or targets that bypass interposed libc calls.

## Features
- Fake wall clock with `--at` or `--offset`.
- Scaled wall-clock progression with `--speed`.
- Scaled wall-clock + sleep pacing with `--hyperspeed`.
- Interactive step control with `--step`:
  - `Enter`: apply configured step.
  - `Left Arrow`: step backward by `-abs(step)`.
  - `Right Arrow`: step forward by `+abs(step)`.
  - `q`: quit step controller.
- Compatibility probe with active shift validation (`--probe`).

## Build
```bash
cargo build
```

Release build:
```bash
cargo build -r
```

## Usage
```bash
timewarp --offset +2h date
timewarp --at "2026-03-01T12:00:00-06:00" your_cmd arg1 arg2
timewarp --speed 10 your_cmd
timewarp --hyperspeed 10 your_cmd
timewarp --step 1s your_cmd
timewarp --probe your_cmd
```

## Install
`cargo install --path .` installs only the `timewarp` binary. The shim library must be installed beside it.

Use the helper script:
```bash
./scripts/install-with-shim.sh
```

## Docker (Linux)
Build and run the Linux integration flow:
```bash
docker build -t timewarp-linux-test .
docker run --rm timewarp-linux-test
```

## Notes
- `--speed` affects wall-clock reads. Short-lived subprocesses may show little visible speed effect because each process starts with a fresh anchor.
- `--hyperspeed` additionally scales common sleep calls (`sleep`, `usleep`, `nanosleep`) to reduce wait time.
