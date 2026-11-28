//! Building the profiles and measuring them: binary sizes on every target, memory on Linux.
//!
//! Everything runs as a process: cargo (or `cargo zigbuild` for the cross targets) builds, the
//! built binaries run, and the memory comes from `/proc/<pid>/status` (`VmRSS`, `VmHWM`).

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Example, Footprint, Profile};
use crate::console::{Process, add_devices, quit_and_read_heap_peak};
use crate::report::{Measurement, Metric};

/// The Cargo profile every measured binary is built with.
pub(crate) const CARGO_PROFILE: &str = "release-small";
/// The package of the service binary.
const SERVICE_PACKAGE: &str = "matter-rs";
/// The service binary.
const SERVICE_BINARY: &str = "matter-rs";

/// How long a node may take to come up.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a node may take to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the memory is left to settle between "running" or "last device served" and the
/// reading: long enough for the first announcements and timers after start, short against the
/// run time.
const SETTLE: Duration = Duration::from_secs(3);
/// The line the service logs once the node is reachable.
const SERVICE_READY: &str = "node is running";
/// The line the example prints once the node is reachable.
const EXAMPLE_READY: &str = "onoff_node: ready";
/// The interface the example announces on, when the environment names one (as in the tests of
/// the engine).
const INTERFACE_ENV: &str = "MATTER_RS_TEST_INTERFACE";

/// The workspace as cargo sees it.
pub(crate) struct Workspace {
    /// The workspace root.
    pub(crate) root: PathBuf,
    /// The target directory, `CARGO_TARGET_DIR` included.
    pub(crate) target_dir: PathBuf,
    cargo: OsString,
}

impl Workspace {
    /// Asks cargo for the target directory of the workspace at `root`.
    pub(crate) fn locate(root: &Path) -> Result<Self, String> {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        let output = Command::new(&cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
            ])
            .arg(root.join("Cargo.toml"))
            .output()
            .map_err(|error| format!("running cargo metadata: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo metadata failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("cargo metadata: {error}"))?;
        let target_dir = metadata["target_directory"]
            .as_str()
            .ok_or_else(|| String::from("cargo metadata: no target_directory"))?;
        Ok(Self {
            root: root.to_owned(),
            target_dir: PathBuf::from(target_dir),
            cargo,
        })
    }

    /// A scratch directory of the measurement, created empty.
    fn scratch(&self, name: &str) -> Result<PathBuf, String> {
        let dir = self.target_dir.join("footprint").join(name);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| format!("clearing {}: {e}", dir.display()))?;
        }
        fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        Ok(dir)
    }

    fn cargo(&self, cross: bool) -> Command {
        let mut command = Command::new(&self.cargo);
        command.current_dir(&self.root);
        command.arg(if cross { "zigbuild" } else { "build" });
        command.args(["--locked", "--profile", CARGO_PROFILE]);
        command
    }

    /// Where cargo puts a binary of `CARGO_PROFILE` for `target` (the host when `None`).
    fn output_dir(&self, target: Option<&str>) -> PathBuf {
        let mut dir = self.target_dir.clone();
        if let Some(target) = target {
            dir.push(target);
        }
        dir.push(CARGO_PROFILE);
        dir
    }
}

/// The cargo arguments that select a profile's features.
pub(crate) fn feature_args(profile: &Profile) -> Vec<String> {
    let mut args = vec![String::from("--package"), String::from(SERVICE_PACKAGE)];
    if !profile.default_features {
        args.push(String::from("--no-default-features"));
    }
    if !profile.features.is_empty() {
        args.push(String::from("--features"));
        args.push(profile.features.join(","));
    }
    args
}

fn run_build(mut command: Command, what: &str) -> Result<(), String> {
    let started = Instant::now();
    eprintln!("footprint: building {what}");
    let status = command
        .status()
        .map_err(|error| format!("building {what}: {error}"))?;
    if !status.success() {
        return Err(format!("building {what} failed ({status})"));
    }
    eprintln!(
        "footprint: built {what} in {} s",
        started.elapsed().as_secs()
    );
    Ok(())
}

fn file_size(path: &Path) -> Result<u64, String> {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// Builds a profile for `target` (the host when `None`), keeps a copy of the binary in the
/// scratch directory (the next profile overwrites cargo's output) and returns its path.
fn build_profile(
    workspace: &Workspace,
    profile: &Profile,
    target: Option<&str>,
) -> Result<PathBuf, String> {
    let mut command = workspace.cargo(target.is_some());
    command.args(feature_args(profile));
    if let Some(target) = target {
        command.args(["--target", target]);
    }
    let target_name = target.unwrap_or("host");
    run_build(command, &format!("{} for {target_name}", profile.name))?;
    let file = if target.is_some() {
        String::from(SERVICE_BINARY)
    } else {
        format!("{SERVICE_BINARY}{}", std::env::consts::EXE_SUFFIX)
    };
    let built = workspace.output_dir(target).join(&file);
    let kept = workspace
        .scratch(&format!("bin-{}-{target_name}", profile.name))?
        .join(&file);
    fs::copy(&built, &kept).map_err(|e| format!("copying {}: {e}", built.display()))?;
    Ok(kept)
}

fn build_example(workspace: &Workspace, example: &Example) -> Result<PathBuf, String> {
    let mut command = workspace.cargo(false);
    command.args(["--package", &example.package, "--example", &example.name]);
    run_build(command, &format!("the example {}", example.name))?;
    Ok(workspace.output_dir(None).join("examples").join(format!(
        "{}{}",
        example.name,
        std::env::consts::EXE_SUFFIX
    )))
}

/// The resident set size now and its peak so far, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Memory {
    /// `VmRSS`.
    pub(crate) rss: u64,
    /// `VmHWM`.
    pub(crate) high_water: u64,
}

/// Reads `VmRSS` and `VmHWM` from the text of `/proc/<pid>/status`.
pub(crate) fn parse_proc_status(text: &str) -> Result<Memory, String> {
    let field = |name: &str| -> Result<u64, String> {
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .ok_or_else(|| format!("/proc status has no {name}"))?;
        let mut words = line.split_whitespace();
        let value: u64 = words
            .next()
            .and_then(|value| value.parse().ok())
            .ok_or_else(|| format!("{name}: {line:?} is not a number"))?;
        match words.next() {
            Some("kB") => value
                .checked_mul(1024)
                .ok_or_else(|| format!("{name}: {value} kB overflows")),
            other => Err(format!("{name}: unexpected unit {other:?}")),
        }
    };
    Ok(Memory {
        rss: field("VmRSS")?,
        high_water: field("VmHWM")?,
    })
}

/// The memory of a running process, from `/proc`.
pub(crate) fn read_memory(pid: u32) -> Result<Memory, String> {
    let path = format!("/proc/{pid}/status");
    let text = fs::read_to_string(&path).map_err(|error| format!("{path}: {error}"))?;
    parse_proc_status(&text)
}

/// Adds `dir = <state>` to the `[state]` table of a starter configuration.
pub(crate) fn with_state_dir(config: &str, state: &Path) -> String {
    let line = format!(
        "dir = {}",
        toml::Value::String(state.to_string_lossy().into_owned())
    );
    let mut out = String::new();
    let mut placed = false;
    for text in config.lines() {
        out.push_str(text);
        out.push('\n');
        if !placed && text.trim() == "[state]" {
            out.push_str(&line);
            out.push('\n');
            placed = true;
        }
    }
    if !placed {
        out.push_str("\n[state]\n");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Starts `matter-rs run` with the starter configuration and no devices, and measures its
/// memory once the node is running and has settled.
fn service_idle_memory(
    workspace: &Workspace,
    profile: &Profile,
    binary: &Path,
) -> Result<Memory, String> {
    let dir = workspace.scratch(&format!("run-{}", profile.name))?;
    let config = dir.join("config.toml");
    let status = Command::new(binary)
        .arg("init")
        .arg("--output")
        .arg(&config)
        .status()
        .map_err(|error| format!("matter-rs init: {error}"))?;
    if !status.success() {
        return Err(format!("matter-rs init failed ({status})"));
    }
    let text = fs::read_to_string(&config).map_err(|e| format!("{}: {e}", config.display()))?;
    fs::write(&config, with_state_dir(&text, &dir.join("state")))
        .map_err(|e| format!("{}: {e}", config.display()))?;

    let mut command = Command::new(binary);
    command
        .arg("run")
        .arg("--config")
        .arg(&config)
        .env_remove("RUST_LOG");
    let mut process = Process::spawn(command)?;
    process
        .console
        .wait_for("ready line of the service", START_TIMEOUT, |line| {
            line.contains(SERVICE_READY)
        })?;
    thread::sleep(SETTLE);
    let memory = read_memory(process.id())?;
    stop_gracefully(&mut process)?;
    Ok(memory)
}

/// Sends SIGTERM through `kill` (no unsafe code for a signal) and waits for the exit.
fn stop_gracefully(process: &mut Process) -> Result<(), String> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(process.id().to_string())
        .status()
        .map_err(|error| format!("kill -TERM: {error}"))?;
    if !status.success() {
        return Err(format!("kill -TERM failed ({status})"));
    }
    let status = process.wait_exit(STOP_TIMEOUT)?;
    if !status.success() {
        return Err(format!("the service did not stop cleanly ({status})"));
    }
    Ok(())
}

/// What a run of the example measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExampleRun {
    /// The devices that were added and answered with `ok`.
    pub(crate) devices: u16,
    /// The memory while all of them were served.
    pub(crate) memory: Memory,
    /// The heap peak the example printed on `quit`.
    pub(crate) heap_peak: u64,
}

/// Starts the example, adds the devices, and measures the memory once the last one is served;
/// then the heap peak it prints on `quit`.
pub(crate) fn run_example(
    binary: &Path,
    example: &Example,
    state: &Path,
    read: impl Fn(u32) -> Result<Memory, String>,
) -> Result<ExampleRun, String> {
    let mut command = Command::new(binary);
    command
        .arg("--state-dir")
        .arg(state)
        .args(["--port", "0", "--no-initial-device", "--heap-stats"])
        .env_remove("RUST_LOG");
    if let Some(interface) = std::env::var(INTERFACE_ENV)
        .ok()
        .filter(|name| !name.is_empty())
    {
        command.args(["--interface", &interface]);
    }
    let mut process = Process::spawn(command)?;
    process
        .console
        .wait_for("ready line of the example", START_TIMEOUT, |line| {
            line.starts_with(EXAMPLE_READY)
        })?;
    let devices = add_devices(
        &mut process.console,
        example.first_endpoint,
        example.devices,
        &example.device_kind,
    )?;
    eprintln!("footprint: {devices} devices served by {}", example.name);
    thread::sleep(SETTLE);
    let memory = read(process.id())?;
    let heap_peak = quit_and_read_heap_peak(&mut process.console)?;
    let status = process.wait_exit(STOP_TIMEOUT)?;
    if !status.success() {
        return Err(format!("{} did not stop cleanly ({status})", example.name));
    }
    Ok(ExampleRun {
        devices,
        memory,
        heap_peak,
    })
}

/// Builds and measures everything: the profiles and the example on the host, and with `cross`
/// the profiles on the cross targets.
pub(crate) fn measure_all(
    workspace: &Workspace,
    footprint: &Footprint,
    cross: bool,
) -> Result<Vec<Measurement>, String> {
    if !cfg!(target_os = "linux") {
        return Err(not_linux_message());
    }
    let host = footprint.host_target.as_str();
    let measurement = |subject: &str, target: &str, metric: Metric, value: u64| Measurement {
        subject: subject.to_owned(),
        target: target.to_owned(),
        metric,
        value,
    };
    let mut measurements = Vec::new();
    let mut binaries = Vec::new();
    for profile in footprint.active_profiles() {
        let binary = build_profile(workspace, profile, None)?;
        measurements.push(measurement(
            &profile.name,
            host,
            Metric::BinarySize,
            file_size(&binary)?,
        ));
        binaries.push((profile, binary));
    }
    let example = &footprint.example;
    let example_binary = build_example(workspace, example)?;
    measurements.push(measurement(
        &example.name,
        host,
        Metric::BinarySize,
        file_size(&example_binary)?,
    ));

    for (profile, binary) in &binaries {
        let memory = service_idle_memory(workspace, profile, binary)?;
        measurements.push(measurement(&profile.name, host, Metric::Rss, memory.rss));
        measurements.push(measurement(
            &profile.name,
            host,
            Metric::RssHighWater,
            memory.high_water,
        ));
    }
    let state = workspace.scratch("example-state")?;
    let run = run_example(&example_binary, example, &state, read_memory)?;
    measurements.push(measurement(
        &example.name,
        host,
        Metric::Rss,
        run.memory.rss,
    ));
    measurements.push(measurement(
        &example.name,
        host,
        Metric::RssHighWater,
        run.memory.high_water,
    ));
    measurements.push(measurement(
        &example.name,
        host,
        Metric::HeapPeak,
        run.heap_peak,
    ));

    if cross {
        for target in &footprint.cross_targets {
            for profile in footprint.active_profiles() {
                let binary = build_profile(workspace, profile, Some(target))?;
                measurements.push(measurement(
                    &profile.name,
                    target,
                    Metric::BinarySize,
                    file_size(&binary)?,
                ));
            }
        }
    }
    Ok(measurements)
}

/// What `footprint` says on a host that is not Linux.
pub(crate) fn not_linux_message() -> String {
    format!(
        "cargo xtask footprint measures on Linux only (memory from /proc); this host is {}. \
         Run it on Linux (the footprint workflow does), or compare a report of such a run with \
         --from-report <report.json>.",
        std::env::consts::OS
    )
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::{Memory, feature_args, parse_proc_status, run_example, with_state_dir};
    use crate::config::{Footprint, Profile};

    #[test]
    fn proc_status_is_read_in_bytes() {
        let text = "Name:\tonoff_node\nVmPeak:\t  900000 kB\nVmHWM:\t   12000 kB\n\
                    VmRSS:\t   11000 kB\nThreads:\t5\n";
        assert_eq!(
            parse_proc_status(text),
            Ok(Memory {
                rss: 11_000 * 1024,
                high_water: 12_000 * 1024
            })
        );
        assert!(
            parse_proc_status("VmRSS:\t1 kB\n")
                .unwrap_err()
                .contains("VmHWM")
        );
        assert!(
            parse_proc_status("VmRSS:\t1 MB\nVmHWM:\t1 kB\n")
                .unwrap_err()
                .contains("unit")
        );
        assert!(
            parse_proc_status("VmRSS:\tx kB\nVmHWM:\t1 kB\n")
                .unwrap_err()
                .contains("not a number")
        );
    }

    #[test]
    fn profiles_select_their_features() {
        let profile = |default_features: bool, features: &[&str]| Profile {
            name: String::from("p"),
            description: String::new(),
            planned: false,
            default_features,
            features: features.iter().map(|&f| f.to_owned()).collect(),
            budget: std::collections::BTreeMap::new(),
        };
        assert_eq!(
            feature_args(&profile(false, &["role-bridge"])),
            [
                "--package",
                "matter-rs",
                "--no-default-features",
                "--features",
                "role-bridge"
            ]
        );
        assert_eq!(
            feature_args(&profile(true, &[])),
            ["--package", "matter-rs"]
        );
        assert_eq!(
            feature_args(&profile(true, &["a", "b"])),
            ["--package", "matter-rs", "--features", "a,b"]
        );
    }

    #[test]
    fn the_state_directory_goes_into_the_state_table() {
        let config = "[network]\nport = 5540\n\n[state]\n# dir = \"/var/lib/matter-rs\"\n\n[log]\n";
        let written = with_state_dir(config, Path::new("/tmp/fp/state"));
        assert_eq!(
            written,
            "[network]\nport = 5540\n\n[state]\ndir = \"/tmp/fp/state\"\n# dir = \"/var/lib/matter-rs\"\n\n[log]\n"
        );
        let appended = with_state_dir("[node]\n", Path::new("s"));
        assert_eq!(appended, "[node]\n\n[state]\ndir = \"s\"\n");
        let parsed: toml::Table = written.parse().expect("still valid TOML");
        assert_eq!(parsed["state"]["dir"].as_str(), Some("/tmp/fp/state"));
    }

    /// The example binary of the dev profile next to this test binary, built on demand
    /// (`cargo test` builds the example as a test harness only).
    fn example_binary(footprint: &Footprint) -> PathBuf {
        let example = &footprint.example;
        let mut dir = std::env::current_exe().expect("the test binary path");
        // target/<profile>/deps/<test> -> target/<profile>
        dir.pop();
        dir.pop();
        let target_dir = dir.parent().expect("the target directory").to_owned();
        let mut build = Command::new(env!("CARGO"));
        build
            .args([
                "build",
                "--package",
                &example.package,
                "--example",
                &example.name,
            ])
            .arg("--manifest-path")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml"))
            .arg("--target-dir")
            .arg(&target_dir);
        if !cfg!(debug_assertions) {
            build.arg("--release");
        }
        let status = build.status().expect("cargo runs");
        assert!(status.success(), "the example builds");
        let binary =
            dir.join("examples")
                .join(format!("{}{}", example.name, std::env::consts::EXE_SUFFIX));
        assert!(binary.is_file(), "example binary at {}", binary.display());
        binary
    }

    /// The real example gets the devices of the committed budget file through its protocol,
    /// answers every `add` with `ok`, and the memory is read while all of them are served. On
    /// Linux the memory comes from /proc; elsewhere the reading is a stand-in that checks the
    /// process is the one that served the devices.
    #[test]
    fn twenty_devices_are_added() {
        let footprint =
            Footprint::parse(crate::config::COMMITTED).expect("footprint.toml is valid");
        assert_eq!(footprint.example.devices, 20);
        let binary = example_binary(&footprint);
        let state = std::env::temp_dir().join(format!("matter-rs-xtask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let read = |pid: u32| {
            assert_ne!(pid, 0);
            if cfg!(target_os = "linux") {
                super::read_memory(pid)
            } else {
                Ok(Memory {
                    rss: 1,
                    high_water: 1,
                })
            }
        };
        let run = run_example(&binary, &footprint.example, &state, read).expect("measured");
        let _ = std::fs::remove_dir_all(&state);
        assert_eq!(run.devices, 20);
        assert!(
            run.memory.rss > 0 && run.memory.high_water >= run.memory.rss,
            "{:?}",
            run.memory
        );
        assert!(run.heap_peak > 0, "heap peak {}", run.heap_peak);
    }
}
