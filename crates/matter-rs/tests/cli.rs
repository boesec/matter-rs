//! Runs the `matter-rs` binary and checks the command-line contract: the subcommands, their
//! output and their exit codes (0 success, 1 failure, 2 usage).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::workspace_root;

const BINARY: &str = env!("CARGO_BIN_EXE_matter-rs");

/// A fresh, empty scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("matter-rs-cli-{}-{name}", std::process::id()));
    if directory.exists() {
        fs::remove_dir_all(&directory).expect("remove an old scratch directory");
    }
    fs::create_dir_all(&directory).expect("create the scratch directory");
    directory
}

/// The binary with a predictable environment: no configuration path from the caller's
/// environment.
fn matter_rs() -> Command {
    let mut command = Command::new(BINARY);
    command.env_remove("MATTER_RS_CONFIG");
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("run the matter-rs binary")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

fn describe(output: &Output) -> String {
    format!(
        "exit code {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        text(&output.stdout),
        text(&output.stderr)
    )
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(output.status.code(), Some(code), "{}", describe(output));
}

fn init(path: &Path) -> Output {
    run(matter_rs().arg("init").arg("--output").arg(path))
}

fn validate(path: &Path) -> Output {
    run(matter_rs().arg("validate").arg(path))
}

/// Passcode and discriminator of a written configuration.
fn secrets_of(path: &Path) -> (i64, i64) {
    let table: toml::Table = fs::read_to_string(path)
        .expect("read the configuration")
        .parse()
        .expect("the configuration is TOML");
    let node = table["node"].as_table().expect("[node] is a table");
    (
        node["passcode"].as_integer().expect("passcode is a number"),
        node["discriminator"]
            .as_integer()
            .expect("discriminator is a number"),
    )
}

#[test]
fn init_writes_valid_config() {
    let directory = scratch("init-valid");
    let path = directory.join("nested").join("config.toml");

    let output = init(&path);
    assert_exit(&output, 0);
    assert!(path.is_file());
    let (passcode, _) = secrets_of(&path);
    assert!(
        !text(&output.stdout).contains(&passcode.to_string()),
        "init must not print the passcode: {}",
        describe(&output)
    );

    let output = validate(&path);
    assert_exit(&output, 0);
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("valid, 0 warnings"),
        "{}",
        describe(&output)
    );
    assert!(
        stdout.contains("state directory: "),
        "{}",
        describe(&output)
    );
    assert!(stdout.contains("sections: "), "{}", describe(&output));
    assert!(
        text(&output.stderr).is_empty(),
        "no errors or warnings expected: {}",
        describe(&output)
    );
    assert!(
        !stdout.contains(&passcode.to_string()),
        "validate must not print the passcode: {}",
        describe(&output)
    );
}

#[cfg(unix)]
#[test]
fn init_writes_a_file_only_the_owner_can_read() {
    use std::os::unix::fs::PermissionsExt;

    let directory = scratch("init-mode");
    let path = directory.join("config.toml");
    assert_exit(&init(&path), 0);
    let mode = fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "mode {mode:o}");
}

#[test]
fn init_generates_fresh_secrets() {
    let directory = scratch("init-fresh");
    let secrets: Vec<(i64, i64)> = (0..4)
        .map(|index| {
            let path = directory.join(format!("config-{index}.toml"));
            assert_exit(&init(&path), 0);
            secrets_of(&path)
        })
        .collect();
    assert_ne!(secrets[0].0, secrets[1].0, "passcodes repeat: {secrets:?}");
    // A discriminator has only 4096 values, so two runs may agree by chance; four runs that all
    // agree would mean the value is not random (chance about 1 in 7e10).
    assert!(
        secrets
            .iter()
            .any(|(_, discriminator)| *discriminator != secrets[0].1),
        "discriminators repeat: {secrets:?}"
    );
}

#[test]
fn init_refuses_overwrite() {
    let directory = scratch("init-overwrite");
    let path = directory.join("config.toml");
    assert_exit(&init(&path), 0);
    let first = fs::read_to_string(&path).unwrap();

    let output = init(&path);
    assert_exit(&output, 1);
    assert!(
        text(&output.stderr).contains("--force"),
        "{}",
        describe(&output)
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), first);

    let output = run(matter_rs()
        .arg("init")
        .arg("--output")
        .arg(&path)
        .arg("--force"));
    assert_exit(&output, 0);
    assert_ne!(fs::read_to_string(&path).unwrap(), first);
}

#[test]
fn validate_reports_every_error_with_its_location() {
    let directory = scratch("validate-invalid");
    let path = directory.join("config.toml");
    fs::write(
        &path,
        "[node]\npasscode = 11111111\ndiscriminator = 5000\ncolour = \"blue\"\n",
    )
    .unwrap();

    let output = validate(&path);
    assert_exit(&output, 1);
    let stderr = text(&output.stderr);
    let name = path.display().to_string();
    for expected in [
        format!("{name}:2:12: error: `node.passcode`"),
        format!("{name}:3:17: error: `node.discriminator` must be between 0 and 4095"),
        format!("{name}:4:1: error: unknown key `colour` in `[node]`"),
    ] {
        assert!(
            stderr.contains(&expected),
            "missing {expected:?}\n{}",
            describe(&output)
        );
    }
    assert!(!stderr.contains("11111111"), "{}", describe(&output));
    assert!(
        text(&output.stdout).contains("invalid, 3 errors, 0 warnings"),
        "{}",
        describe(&output)
    );
}

#[test]
fn validate_reports_warnings_and_still_succeeds() {
    let directory = scratch("validate-warning");
    let path = directory.join("config.toml");
    fs::write(&path, "[node]\npasscode = 20202021\ndiscriminator = 3840\n").unwrap();

    let output = validate(&path);
    assert_exit(&output, 0);
    assert!(
        text(&output.stderr).contains(":2:12: warning: `node.passcode` is the well-known"),
        "{}",
        describe(&output)
    );
    assert!(!text(&output.stderr).contains("20202021"));
}

#[test]
fn validate_unreadable_file_exits_1() {
    let directory = scratch("validate-missing");
    let output = validate(&directory.join("missing.toml"));
    assert_exit(&output, 1);
    assert!(
        text(&output.stderr).contains("cannot read the configuration file"),
        "{}",
        describe(&output)
    );
}

#[test]
fn usage_errors_exit_2() {
    assert_exit(&run(matter_rs().arg("validate").arg("--no-such-option")), 2);
    assert_exit(&run(&mut matter_rs()), 2);
    assert_exit(&run(matter_rs().arg("no-such-command")), 2);
    assert_exit(
        &run(matter_rs().args(["validate", "a.toml", "--config", "b.toml"])),
        2,
    );
}

#[test]
fn validate_finds_the_file_by_option_then_environment() {
    let directory = scratch("validate-lookup");
    let valid = directory.join("valid.toml");
    let invalid = directory.join("invalid.toml");
    assert_exit(&init(&valid), 0);
    fs::write(&invalid, "[node]\n").unwrap();

    let output = run(matter_rs().arg("validate").env("MATTER_RS_CONFIG", &valid));
    assert_exit(&output, 0);
    assert!(
        text(&output.stdout).contains(&valid.display().to_string()),
        "{}",
        describe(&output)
    );

    let output = run(matter_rs()
        .arg("validate")
        .arg("--config")
        .arg(&invalid)
        .env("MATTER_RS_CONFIG", &valid));
    assert_exit(&output, 1);
    assert!(
        text(&output.stderr).contains(&invalid.display().to_string()),
        "{}",
        describe(&output)
    );
}

#[test]
fn features_matches_cfg() {
    let output = run(matter_rs().arg("features"));
    assert_exit(&output, 0);
    let listed: Vec<String> = text(&output.stdout).lines().map(str::to_owned).collect();
    let mut expected = Vec::new();
    if cfg!(feature = "role-bridge") {
        expected.push("role-bridge");
    }
    if cfg!(feature = "adapter-mqtt") {
        expected.push("adapter-mqtt");
    }
    assert_eq!(listed, expected);
}

#[test]
#[cfg_attr(
    not(all(feature = "role-bridge", feature = "adapter-mqtt")),
    ignore = "the committed documents describe a build with every role and adapter"
)]
fn schema_prints_the_committed_documents() {
    let root = workspace_root();
    let output = run(matter_rs().arg("schema"));
    assert_exit(&output, 0);
    let committed = fs::read_to_string(root.join("doc/config.schema.json")).unwrap();
    assert_eq!(text(&output.stdout), committed.replace("\r\n", "\n"));

    let output = run(matter_rs().args(["schema", "--markdown"]));
    assert_exit(&output, 0);
    let committed = fs::read_to_string(root.join("doc/config.md")).unwrap();
    assert_eq!(text(&output.stdout), committed.replace("\r\n", "\n"));
}
