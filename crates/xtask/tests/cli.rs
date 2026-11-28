//! Runs the built `xtask` binary: the exit code follows the budgets, the reports are written,
//! `--bless` writes budgets with the margin and refuses to run in CI, and a measurement off
//! Linux says that it measures on Linux.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const FIXTURE_BUDGETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/footprint.toml");
const FIXTURE_REPORT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/report.json");

/// A fresh, empty scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("matter-rs-xtask-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create the scratch directory");
    dir
}

fn xtask(args: &[&str], config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
    command
        .arg("footprint")
        .args(args)
        .arg("--config")
        .arg(config);
    command.env_remove("CI");
    command
}

fn run(mut command: Command) -> (Option<i32>, String, String) {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().expect("the xtask runs");
    (
        status.code(),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    )
}

#[test]
fn exceeded_budget_exits_with_1() {
    let (code, stdout, stderr) = run(xtask(
        &["--from-report", FIXTURE_REPORT],
        Path::new(FIXTURE_BUDGETS),
    ));
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("**Failed**: 8 within budget, 1 exceeded"),
        "{stdout}"
    );
    assert!(
        stdout.contains("| full-bridge | x86_64-unknown-linux-gnu | stripped binary |"),
        "{stdout}"
    );
    assert!(stderr.contains("a budget is exceeded"), "{stderr}");
}

#[test]
fn budgets_that_hold_exit_with_0_and_write_both_reports() {
    let dir = scratch("within");
    let config = dir.join("footprint.toml");
    let text = fs::read_to_string(FIXTURE_BUDGETS).expect("the fixture");
    // The full bridge's x86_64 binary budget is the first 5_000_000 in the fixture.
    fs::write(
        &config,
        text.replacen(
            "max_binary_bytes = 5_000_000",
            "max_binary_bytes = 5_200_000",
            1,
        ),
    )
    .expect("write");
    let markdown = dir.join("report.md");
    let json = dir.join("report.json");
    let (code, stdout, stderr) = run(xtask(
        &[
            "--from-report",
            FIXTURE_REPORT,
            "--markdown",
            markdown.to_str().expect("utf-8 path"),
            "--json",
            json.to_str().expect("utf-8 path"),
        ],
        &config,
    ));
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    assert!(
        stdout.starts_with("## Footprint\n\nPassed: 9 within budget"),
        "{stdout}"
    );
    assert!(stdout.contains("Measured in 412 s"), "{stdout}");
    assert_eq!(
        fs::read_to_string(&markdown).expect("markdown report"),
        stdout
    );
    let report: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&json).expect("json report")).expect("JSON");
    assert_eq!(report["measurements"].as_array().map(Vec::len), Some(13));
    assert_eq!(report["rows"].as_array().map(Vec::len), Some(14));
    assert_eq!(report["rows"][13]["verdict"], "not_measured");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bless_writes_the_measured_values_with_the_margin() {
    let dir = scratch("bless");
    let config = dir.join("footprint.toml");
    fs::copy(FIXTURE_BUDGETS, &config).expect("copy");
    let (code, stdout, stderr) = run(xtask(
        &["--from-report", FIXTURE_REPORT, "--bless"],
        &config,
    ));
    assert_eq!(code, Some(0), "{stdout}\n{stderr}");
    let written = fs::read_to_string(&config).expect("the budget file");
    // 3_000_000 + 15 % = 3_450_000, rounded up to a whole KiB.
    assert!(
        written.contains("max_binary_bytes = 3_450_880"),
        "{written}"
    );
    // The example's binary had no budget and gets one; 2_500_000 + 15 % -> 2_875_392.
    assert!(
        written.contains("max_binary_bytes = 2_875_392"),
        "{written}"
    );
    // The full bridge's armv7 binary was not measured and keeps its budget.
    assert!(
        written.contains("max_binary_bytes = 4_500_000"),
        "{written}"
    );

    // With another margin.
    fs::copy(FIXTURE_BUDGETS, &config).expect("copy");
    let (code, _, stderr) = run(xtask(
        &["--from-report", FIXTURE_REPORT, "--bless", "--margin", "0"],
        &config,
    ));
    assert_eq!(code, Some(0), "{stderr}");
    let written = fs::read_to_string(&config).expect("the budget file");
    assert!(
        written.contains("max_binary_bytes = 3_000_320"),
        "{written}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn bless_refuses_to_run_in_ci() {
    let dir = scratch("bless-ci");
    let config = dir.join("footprint.toml");
    fs::copy(FIXTURE_BUDGETS, &config).expect("copy");
    let mut command = xtask(&["--from-report", FIXTURE_REPORT, "--bless"], &config);
    command.env("CI", "true");
    let (code, stdout, stderr) = run(command);
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("refuses to run in CI"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&config).expect("the budget file"),
        fs::read_to_string(FIXTURE_BUDGETS).expect("the fixture"),
        "the budget file is unchanged"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn margin_needs_bless() {
    let (code, _, stderr) = run(xtask(
        &["--from-report", FIXTURE_REPORT, "--margin", "5"],
        Path::new(FIXTURE_BUDGETS),
    ));
    assert_eq!(code, Some(2), "{stderr}");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn measuring_off_linux_says_it_measures_on_linux() {
    let (code, stdout, stderr) = run(xtask(&[], Path::new(FIXTURE_BUDGETS)));
    assert_eq!(code, Some(1), "{stdout}\n{stderr}");
    assert!(stderr.contains("measures on Linux only"), "{stderr}");
    assert!(stderr.contains("--from-report"), "{stderr}");
}
