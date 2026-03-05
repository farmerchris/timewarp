use std::path::PathBuf;
use std::process::Command;

fn debug_bin(name: &str) -> PathBuf {
    PathBuf::from("target").join("debug").join(name)
}

fn run_cmd(mut cmd: Command) -> String {
    let out = cmd.output().expect("command should run");
    assert!(
        out.status.success(),
        "command failed: status={:?} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn parse_epoch(output: &str, key: &str) -> i64 {
    let prefix = format!("{key}=");
    output
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .expect("key present")
        .trim()
        .parse::<i64>()
        .expect("key should contain an integer epoch")
}

#[test]
fn now_prints_expected_fields() {
    let cmd = Command::new(debug_bin("now"));
    let stdout = run_cmd(cmd);
    assert!(stdout.contains("time="));
    assert!(stdout.contains("gettimeofday="));
    assert!(stdout.contains("clock_gettime="));
    assert!(stdout.contains("formatted_utc="));
}

#[test]
fn timewarp_help_includes_core_flags() {
    let mut cmd = Command::new(debug_bin("timewarp"));
    cmd.arg("--help");
    let stdout = run_cmd(cmd);

    for flag in ["--at", "--offset", "--speed", "--step", "--probe"] {
        assert!(stdout.contains(flag), "help missing {flag}");
    }
}

#[test]
fn timewarp_runs_now_bin() {
    let mut cmd = Command::new(debug_bin("timewarp"));
    cmd.arg(debug_bin("now"));
    let stdout = run_cmd(cmd);
    assert!(stdout.contains("formatted_utc="));
}

#[test]
fn offset_changes_reported_time() {
    let mut probe_cmd = Command::new(debug_bin("timewarp"));
    probe_cmd.args(["--probe"]);
    probe_cmd.arg(debug_bin("now"));
    let probe_out = probe_cmd.output().expect("probe should run");
    assert!(
        probe_out.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&probe_out.stderr)
    );

    let base_cmd = Command::new(debug_bin("now"));
    let base_stdout = run_cmd(base_cmd);
    let base_time = parse_epoch(&base_stdout, "time");

    let mut warped_cmd = Command::new(debug_bin("timewarp"));
    warped_cmd.args(["--offset", "+2h"]);
    warped_cmd.arg(debug_bin("now"));
    let warped_stdout = run_cmd(warped_cmd);
    let warped_time = parse_epoch(&warped_stdout, "time");

    assert!(
        warped_time >= base_time + 7190 && warped_time <= base_time + 7210,
        "expected about +7200s shift, base={base_time} warped={warped_time}"
    );
}
