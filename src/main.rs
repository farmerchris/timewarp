use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use clap::{ArgGroup, Parser};
use std::fs;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(
    name = "timewarp",
    version,
    about = "Run a process under virtual wall-clock time",
    trailing_var_arg = true,
    group = ArgGroup::new("base_time").args(["at", "offset"]).multiple(false),
    group = ArgGroup::new("rate_mode").args(["speed", "hyperspeed"]).multiple(false)
)]
struct Cli {
    #[arg(
        long,
        help = "Set virtual wall-clock time (RFC3339), e.g. 2026-03-01T12:00:00-06:00"
    )]
    at: Option<String>,

    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Offset wall-clock time, e.g. +2h, -30m, +1h15m"
    )]
    offset: Option<String>,

    #[arg(
        long,
        default_value_t = 1.0,
        help = "Scale wall-clock elapsed time only, e.g. 10.0"
    )]
    speed: f64,

    #[arg(
        long,
        help = "Scale both wall-clock time and sleep durations, e.g. 10.0"
    )]
    hyperspeed: Option<f64>,

    #[arg(long, help = "Also warp monotonic clocks (risky; may break timeouts)")]
    warp_monotonic: bool,

    #[arg(
        long,
        allow_hyphen_values = true,
        help = "Interactive stepping increment, e.g. 1m, 500ms"
    )]
    step: Option<String>,

    #[arg(
        long,
        help = "Run compatibility probe (includes active time-shift check) and fail if ineffective"
    )]
    probe: bool,

    #[arg(required = true)]
    command: Vec<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("timewarp: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    if cli.speed < 0.0 {
        bail!("--speed must be >= 0");
    }
    if let Some(hs) = cli.hyperspeed {
        if hs < 0.0 {
            bail!("--hyperspeed must be >= 0");
        }
    }
    let effective_speed = cli.hyperspeed.unwrap_or(cli.speed);

    let initial_offset_ns = initial_offset_ns(&cli)?;
    let state_path = make_state_path()?;
    write_state(&state_path, initial_offset_ns, effective_speed)?;

    let shim_path = find_shim_path()?;

    if cli.probe {
        run_probe(&cli.command[0], &shim_path, cli.warp_monotonic)?;
    }

    let mut cmd = Command::new(&cli.command[0]);
    cmd.args(&cli.command[1..]);
    cmd.env("TW_STATE_PATH", &state_path);
    if cli.warp_monotonic {
        cmd.env("TW_WARP_MONOTONIC", "1");
    }
    if cli.hyperspeed.is_some() && (effective_speed - 1.0).abs() > f64::EPSILON {
        cmd.env("TW_SCALE_SLEEP", "1");
    }
    inject_preload_env(&mut cmd, &shim_path)?;

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to start command: {}", cli.command[0]))?;

    if let Some(step) = &cli.step {
        let step_ns = parse_duration_ns(step)?;
        let step_abs_ns = step_ns.abs();
        let left = format!("Left Arrow=-{}", format_duration_ns(step_abs_ns));
        let right = format!("Right Arrow=+{}", format_duration_ns(step_abs_ns));
        if step_ns < 0 {
            eprintln!(
                "timewarp step mode: {}/Enter={} | {} | q=quit step control",
                left,
                format_duration_ns(step_ns),
                right
            );
        } else {
            eprintln!(
                "timewarp step mode: {} | {}/Enter={} | q=quit step control",
                left,
                right,
                format_duration_ns(step_ns)
            );
        }
        let state = state_path.clone();
        let speed = effective_speed;
        let initial = initial_offset_ns;
        std::thread::spawn(move || {
            if let Err(err) = run_step_controller(&state, speed, initial, step_ns) {
                eprintln!("timewarp: step mode controller exited: {err:#}");
            }
        });
    }

    let status = child.wait().context("failed waiting for child process")?;
    let _ = fs::remove_file(&state_path);

    Ok(match status.code() {
        Some(code) => ExitCode::from(code as u8),
        None => ExitCode::from(1),
    })
}

fn initial_offset_ns(cli: &Cli) -> Result<i128> {
    if let Some(at) = &cli.at {
        let target: DateTime<Utc> = DateTime::parse_from_rfc3339(at)
            .with_context(|| format!("invalid --at timestamp: {at}"))?
            .with_timezone(&Utc);
        let now = now_unix_ns()?;
        let target_ns = target
            .timestamp_nanos_opt()
            .context("--at timestamp is out of supported range")? as i128;
        Ok(target_ns - now)
    } else if let Some(offset) = &cli.offset {
        parse_duration_ns(offset)
    } else {
        Ok(0)
    }
}

fn now_unix_ns() -> Result<i128> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX_EPOCH")?;
    Ok(now.as_nanos() as i128)
}

fn parse_duration_ns(input: &str) -> Result<i128> {
    let s = input.trim();
    if s.is_empty() {
        bail!("empty duration");
    }

    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1_i128, &s[1..]),
        b'-' => (-1_i128, &s[1..]),
        _ => (1_i128, s),
    };

    if rest.is_empty() {
        bail!("duration has no value");
    }

    let mut i = 0;
    let bytes = rest.as_bytes();
    let mut total: i128 = 0;

    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if start == i {
            bail!("expected number at: {}", &rest[start..]);
        }
        let value: i128 = rest[start..i]
            .parse()
            .with_context(|| format!("invalid number: {}", &rest[start..i]))?;

        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        if unit_start == i {
            bail!("missing unit after {}", &rest[start..i]);
        }
        let unit = &rest[unit_start..i];

        let factor: i128 = match unit {
            "ns" => 1,
            "us" => 1_000,
            "ms" => 1_000_000,
            "s" => 1_000_000_000,
            "m" => 60 * 1_000_000_000,
            "h" => 60 * 60 * 1_000_000_000,
            "d" => 24 * 60 * 60 * 1_000_000_000,
            _ => bail!("unsupported duration unit: {unit}"),
        };

        total = total
            .checked_add(value.checked_mul(factor).context("duration overflow")?)
            .context("duration overflow")?;
    }

    Ok(sign * total)
}

fn format_duration_ns(ns: i128) -> String {
    let sign = if ns < 0 { "-" } else { "" };
    let mut rem = ns.abs();

    let d = rem / 86_400_000_000_000;
    rem %= 86_400_000_000_000;
    let h = rem / 3_600_000_000_000;
    rem %= 3_600_000_000_000;
    let m = rem / 60_000_000_000;
    rem %= 60_000_000_000;
    let s = rem / 1_000_000_000;
    rem %= 1_000_000_000;

    let mut out = String::new();
    if d > 0 {
        out.push_str(&format!("{d}d"));
    }
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 {
        out.push_str(&format!("{s}s"));
    }
    if rem > 0 || out.is_empty() {
        out.push_str(&format!("{}ns", rem));
    }

    format!("{sign}{out}")
}

fn make_state_path() -> Result<PathBuf> {
    let pid = std::process::id();
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before epoch")?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!("timewarp-{pid}-{n}.state")))
}

fn write_state(path: &Path, offset_ns: i128, speed: f64) -> Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, format!("{offset_ns} {speed}\n"))
        .with_context(|| format!("failed writing {}", tmp.display()))?;
    fs::rename(&tmp, path)
        .with_context(|| format!("failed moving {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn find_shim_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let dir = exe
        .parent()
        .context("failed to resolve executable directory")?;

    #[cfg(target_os = "linux")]
    let name = "libtimewarp_shim.so";
    #[cfg(target_os = "macos")]
    let name = "libtimewarp_shim.dylib";
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    compile_error!("timewarp supports only linux and macos");

    let path = dir.join(name);
    if path.exists() {
        Ok(path)
    } else {
        bail!(
            "shim library not found at {}. Build with cargo build first.",
            path.display()
        )
    }
}

fn inject_preload_env(cmd: &mut Command, shim_path: &Path) -> Result<()> {
    let shim = shim_path
        .to_str()
        .context("shim path is not valid UTF-8")?
        .to_owned();

    #[cfg(target_os = "linux")]
    {
        let key = "LD_PRELOAD";
        let merged = match std::env::var(key) {
            Ok(prev) if !prev.trim().is_empty() => format!("{shim}:{prev}"),
            _ => shim,
        };
        cmd.env(key, merged);
    }

    #[cfg(target_os = "macos")]
    {
        let key = "DYLD_INSERT_LIBRARIES";
        let merged = match std::env::var(key) {
            Ok(prev) if !prev.trim().is_empty() => format!("{shim}:{prev}"),
            _ => shim,
        };
        cmd.env(key, merged);
        cmd.env("DYLD_FORCE_FLAT_NAMESPACE", "1");
    }

    Ok(())
}

fn run_probe(program: &str, shim_path: &Path, warp_monotonic: bool) -> Result<()> {
    let resolved = resolve_command_path(program)?;
    let meta = fs::metadata(&resolved)
        .with_context(|| format!("failed to stat {}", resolved.display()))?;
    let mode = meta.permissions().mode();

    eprintln!("timewarp probe: binary: {}", resolved.display());

    if mode & 0o4000 != 0 {
        eprintln!("timewarp probe: warning: setuid bit is set; preload injection may be blocked");
    }

    let output = Command::new("file")
        .arg("-Lb")
        .arg(&resolved)
        .output()
        .context("failed to run 'file' for compatibility probe")?;

    if output.status.success() {
        let desc = String::from_utf8_lossy(&output.stdout);
        eprintln!("timewarp probe: type: {}", desc.trim());
        let lower = desc.to_ascii_lowercase();
        if lower.contains("statically linked") {
            eprintln!(
                "timewarp probe: warning: appears statically linked; LD_PRELOAD/DYLD_INSERT_LIBRARIES likely will not work"
            );
        }
        if lower.contains("go buildid") {
            eprintln!("timewarp probe: note: Go binaries are often static or partially static");
        }
    }

    run_active_probe(shim_path, warp_monotonic)?;

    Ok(())
}

fn resolve_command_path(program: &str) -> Result<PathBuf> {
    let p = Path::new(program);
    if p.components().count() > 1 || p.is_absolute() {
        Ok(p.to_path_buf())
    } else {
        which::which(program)
            .with_context(|| format!("failed to resolve command in PATH: {program}"))
    }
}

fn run_active_probe(shim_path: &Path, warp_monotonic: bool) -> Result<()> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let now_bin = exe
        .parent()
        .context("failed to resolve executable directory")?
        .join("now");

    if !now_bin.exists() {
        eprintln!(
            "timewarp probe: warning: active probe skipped (missing helper binary: {})",
            now_bin.display()
        );
        return Ok(());
    }

    let base = Command::new(&now_bin)
        .output()
        .context("active probe failed to run baseline helper")?;
    if !base.status.success() {
        bail!(
            "active probe baseline failed: {}",
            String::from_utf8_lossy(&base.stderr).trim()
        );
    }
    let base_stdout = String::from_utf8_lossy(&base.stdout);
    let base_epoch = parse_epoch_line(&base_stdout, "time")
        .context("active probe baseline output missing time=<epoch>")?;

    let state_path = make_state_path()?;
    write_state(&state_path, 3_600_000_000_000, 1.0)?;

    let mut warped_cmd = Command::new(&now_bin);
    warped_cmd.env("TW_STATE_PATH", &state_path);
    if warp_monotonic {
        warped_cmd.env("TW_WARP_MONOTONIC", "1");
    }
    inject_preload_env(&mut warped_cmd, shim_path)?;
    let warped = warped_cmd
        .output()
        .context("active probe failed to run warped helper")?;
    let _ = fs::remove_file(&state_path);

    if !warped.status.success() {
        bail!(
            "active probe warped run failed: {}",
            String::from_utf8_lossy(&warped.stderr).trim()
        );
    }

    let warped_stdout = String::from_utf8_lossy(&warped.stdout);
    let warped_epoch = parse_epoch_line(&warped_stdout, "time")
        .context("active probe warped output missing time=<epoch>")?;
    let delta = warped_epoch - base_epoch;
    eprintln!("timewarp probe: active shift check delta={}s", delta);

    if !(3590..=3610).contains(&delta) {
        bail!(
            "active probe failed: expected ~+3600s shift, got {delta}s. Interposition is ineffective on this target."
        );
    }

    Ok(())
}

fn parse_epoch_line(output: &str, key: &str) -> Result<i64> {
    let prefix = format!("{key}=");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .context("key not found")?
        .trim()
        .parse::<i64>()
        .context("invalid epoch")
}

fn run_step_controller(state_path: &Path, speed: f64, mut current_offset_ns: i128, step_ns: i128) -> Result<()> {
    let step_abs_ns = step_ns.abs();
    let mut tty = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .context("step mode requires an interactive TTY")?;
    let _raw = RawModeGuard::new(tty.as_raw_fd())?;

    loop {
        let mut b = [0_u8; 1];
        if tty.read_exact(&mut b).is_err() {
            break;
        }

        let delta = match b[0] {
            b'\n' | b'\r' => Some(step_ns),
            b'q' | b'Q' => break,
            0x1b => {
                let mut seq = [0_u8; 2];
                if tty.read_exact(&mut seq).is_err() {
                    break;
                }
                if seq[0] == b'[' {
                    match seq[1] {
                        b'D' => Some(-step_abs_ns),
                        b'C' => Some(step_abs_ns),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        let Some(delta) = delta else {
            continue;
        };

        current_offset_ns += delta;
        write_state(state_path, current_offset_ns, speed)?;
        eprintln!("timewarp: offset now {}", format_duration_ns(current_offset_ns));
    }

    Ok(())
}

struct RawModeGuard {
    fd: i32,
    saved: libc::termios,
}

impl RawModeGuard {
    fn new(fd: i32) -> Result<Self> {
        let mut saved = unsafe { std::mem::zeroed::<libc::termios>() };
        let rc = unsafe { libc::tcgetattr(fd, &mut saved as *mut libc::termios) };
        if rc != 0 {
            return Err(io::Error::last_os_error()).context("tcgetattr failed");
        }

        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_iflag &= !(libc::IXON | libc::ICRNL);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;

        let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw as *const libc::termios) };
        if rc != 0 {
            return Err(io::Error::last_os_error()).context("tcsetattr failed");
        }

        Ok(Self { fd, saved })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved as *const libc::termios);
        }
    }
}
