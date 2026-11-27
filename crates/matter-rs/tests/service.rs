//! Runs `matter-rs run` as a real process: start-up, the log it writes, start-up errors and the
//! clean stop on a signal.
//!
//! Every test uses its own UDP port and its own state directory, so the tests run in parallel.
//! The nodes listen on IPv4 only, which every CI runner has.

use std::fs;
use std::io::{BufRead as _, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_matter-rs");
/// How long a debug build may take from start to the ready line.
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a stop may take; the stop timeout of the systemd unit.
#[cfg_attr(not(unix), allow(dead_code))]
const STOP_TIMEOUT: Duration = Duration::from_secs(20);
/// The line the service logs when the node is reachable.
const READY: &str = "node is running";
/// A valid passcode that is neither a test passcode nor a trivial one.
const PASSCODE: u32 = 73_914_826;

/// A fresh, empty scratch directory for one test.
fn scratch(name: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("matter-rs-service-{}-{name}", std::process::id()));
    if directory.exists() {
        fs::remove_dir_all(&directory).expect("remove an old scratch directory");
    }
    fs::create_dir_all(&directory).expect("create the scratch directory");
    directory
}

/// Writes a configuration with the bridge role, a plain passcode and the state directory next
/// to it, readable by its owner only.
fn write_config(directory: &Path, port: u16, format: &str, level: &str) -> PathBuf {
    let path = directory.join("config.toml");
    let text = format!(
        "[node]\npasscode = {PASSCODE}\ndiscriminator = 2049\n\n\
         [network]\nipv4 = true\nipv6 = false\nport = {port}\n\n\
         [state]\ndir = \"state\"\n\n\
         [log]\nlevel = \"{level}\"\nformat = \"{format}\"\n\n\
         [bridge]\n"
    );
    write_private(&path, &text);
    path
}

fn write_private(path: &Path, text: &str) {
    fs::write(path, text).expect("write the configuration");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// The binary with a predictable environment.
fn matter_rs() -> Command {
    let mut command = Command::new(BINARY);
    command
        .env_remove("MATTER_RS_CONFIG")
        .env_remove("RUST_LOG");
    command
}

/// The output lines of a running service, standard output and standard error together.
#[derive(Default)]
struct Lines {
    lines: Mutex<(Vec<String>, usize)>,
    changed: Condvar,
}

impl Lines {
    fn push(&self, line: Option<String>) {
        let mut guard = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
        match line {
            Some(line) => guard.0.push(line),
            None => guard.1 += 1,
        }
        self.changed.notify_all();
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .0
            .clone()
    }
}

/// A running `matter-rs run`.
struct Service {
    child: Child,
    output: Arc<Lines>,
    readers: Vec<thread::JoinHandle<()>>,
}

impl Service {
    fn start(config: &Path) -> Self {
        let mut child = matter_rs()
            .arg("run")
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start matter-rs run");
        let output = Arc::new(Lines::default());
        let stdout: Box<dyn Read + Send> = Box::new(child.stdout.take().unwrap());
        let stderr: Box<dyn Read + Send> = Box::new(child.stderr.take().unwrap());
        let readers = [stdout, stderr]
            .into_iter()
            .map(|stream| {
                let output = Arc::clone(&output);
                thread::spawn(move || {
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { break };
                        output.push(Some(line));
                    }
                    output.push(None);
                })
            })
            .collect();
        Self {
            child,
            output,
            readers,
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn describe(&self) -> String {
        self.output.snapshot().join("\n")
    }

    /// Waits until a line contains `needle`; panics with the output when it does not appear
    /// in time or the output ends first.
    fn wait_for(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        let mut guard = self
            .output
            .lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(line) = guard.0.iter().find(|line| line.contains(needle)) {
                return line.clone();
            }
            let now = Instant::now();
            assert!(
                guard.1 < 2 && now < deadline,
                "{needle:?} did not appear; output:\n{}",
                guard.0.join("\n")
            );
            guard = self
                .output
                .changed
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Waits for the process to exit within `timeout` and returns its status and output.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn wait_exit(mut self, timeout: Duration) -> (ExitStatus, Vec<String>, Duration) {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll the process") {
                for reader in self.readers.drain(..) {
                    reader.join().expect("the output reader");
                }
                return (status, self.output.snapshot(), started.elapsed());
            }
            if started.elapsed() > timeout {
                let _ = self.child.kill();
                panic!(
                    "matter-rs did not exit within {timeout:?}; output:\n{}",
                    self.describe()
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Kills the process and returns its output.
    fn kill(mut self) -> Vec<String> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            reader.join().expect("the output reader");
        }
        self.output.snapshot()
    }

    #[cfg(unix)]
    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("run kill");
        assert!(status.success(), "kill -{name} failed");
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs a short command of the binary and returns its exit code and output.
fn run_once(command: &mut Command) -> (Option<i32>, String) {
    let output = command.output().expect("run matter-rs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.code(), text.replace("\r\n", "\n"))
}

/// The manual pairing code and the QR text of a configuration, from `pairing-info`.
fn pairing_codes(config: &Path) -> (String, String) {
    let (code, text) = run_once(matter_rs().arg("pairing-info").arg(config));
    assert_eq!(code, Some(0), "{text}");
    let value = |prefix: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("{prefix:?} missing in {text}"))
            .trim()
            .to_owned()
    };
    (value("manual pairing code:"), value("qr code text:"))
}

/// The unique id the service keeps in the state directory.
fn stored_unique_id(state: &Path) -> Option<Vec<u8>> {
    use matter_rs_engine::Storage as _;
    matter_rs_engine::DirectoryStorage::open(state)
        .expect("open the state directory")
        .load("service/unique-id")
        .expect("read the unique id")
}

#[cfg(unix)]
#[test]
fn run_starts_and_stops_on_signal() {
    let directory = scratch("signal");
    let config = write_config(&directory, 45_611, "text", "info");

    for signal in ["TERM", "INT"] {
        let service = Service::start(&config);
        service.wait_for(READY, START_TIMEOUT);
        service.wait_for("matter-rs pairing-info", START_TIMEOUT);
        service.signal(signal);
        let (status, lines, elapsed) = service.wait_exit(STOP_TIMEOUT);
        let output = lines.join("\n");
        assert_eq!(status.code(), Some(0), "SIG{signal}: {output}");
        assert!(elapsed < STOP_TIMEOUT, "SIG{signal}: took {elapsed:?}");
        for expected in [
            format!("stop signal received, shutting down signal=\"SIG{signal}\""),
            String::from("node stopped"),
            String::from("matter-rs stopped"),
        ] {
            assert!(
                output.contains(&expected),
                "{expected:?} missing:\n{output}"
            );
        }
    }
}

#[test]
fn run_keeps_the_unique_id_across_restarts() {
    let directory = scratch("unique-id");
    let config = write_config(&directory, 45_617, "text", "info");
    let state = directory.join("state");
    let mut ids = Vec::new();
    for _ in 0..2 {
        let service = Service::start(&config);
        service.wait_for(READY, START_TIMEOUT);
        service.kill();
        ids.push(stored_unique_id(&state).expect("the service stores a unique id"));
    }
    assert_eq!(ids[0].len(), 32, "{:?}", String::from_utf8_lossy(&ids[0]));
    assert_eq!(ids[0], ids[1]);
}

#[cfg(unix)]
#[test]
fn run_exits_1_on_a_second_signal() {
    let directory = scratch("second-signal");
    let config = write_config(&directory, 45_612, "text", "info");
    let service = Service::start(&config);
    service.wait_for(READY, START_TIMEOUT);
    service.signal("TERM");
    // The engine keeps the node running for half a second while it shuts down; the second
    // signal arrives in that time.
    service.wait_for("shutting down", STOP_TIMEOUT);
    service.signal("TERM");
    let (status, lines, _) = service.wait_exit(STOP_TIMEOUT);
    let output = lines.join("\n");
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(output.contains("second stop signal"), "{output}");
}

#[test]
fn run_reports_startup_errors() {
    let directory = scratch("startup-errors");

    // An invalid configuration: the errors as `validate` prints them, exit 1.
    let invalid = directory.join("invalid.toml");
    write_private(
        &invalid,
        "[node]\npasscode = 11111111\ndiscriminator = 5000\n",
    );
    let (code, text) = run_once(matter_rs().arg("run").arg("--config").arg(&invalid));
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("error: `node.passcode`"), "{text}");
    assert!(text.contains("is not valid"), "{text}");
    assert!(!text.contains("11111111"), "{text}");

    // A missing file.
    let (code, text) = run_once(
        matter_rs()
            .arg("run")
            .arg("--config")
            .arg(directory.join("missing.toml")),
    );
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains("cannot read the configuration file"),
        "{text}"
    );

    // No role enabled.
    let no_role = directory.join("no-role.toml");
    write_private(
        &no_role,
        &format!("[node]\npasscode = {PASSCODE}\ndiscriminator = 2049\n[state]\ndir = \"s\"\n"),
    );
    let (code, text) = run_once(matter_rs().arg("run").arg("--config").arg(&no_role));
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("no role is enabled"), "{text}");

    // An invalid log filter in the environment.
    let config = write_config(&directory, 45_613, "text", "info");
    let (code, text) = run_once(
        matter_rs()
            .arg("run")
            .arg("--config")
            .arg(&config)
            .env("RUST_LOG", "rs_matter=loud"),
    );
    assert_eq!(code, Some(1), "{text}");
    assert!(
        text.contains("RUST_LOG is not a valid log filter"),
        "{text}"
    );

    // The Matter port is taken: the engine cannot bind it.
    let blocker = std::net::UdpSocket::bind(("0.0.0.0", 45_613)).expect("bind the port");
    let started = Instant::now();
    let (code, text) = run_once(matter_rs().arg("run").arg("--config").arg(&config));
    assert_eq!(code, Some(1), "{text}");
    assert!(text.contains("the node did not start"), "{text}");
    assert!(text.contains("Matter socket"), "{text}");
    assert!(started.elapsed() < START_TIMEOUT, "{:?}", started.elapsed());
    drop(blocker);
}

#[test]
fn log_never_contains_secrets() {
    for format in ["text", "json"] {
        let directory = scratch(&format!("secrets-{format}"));
        let port = if format == "text" { 45_614 } else { 45_615 };
        let config = write_config(&directory, port, format, "debug");
        let (manual_code, qr_text) = pairing_codes(&config);

        let service = Service::start(&config);
        service.wait_for(READY, START_TIMEOUT);
        service.wait_for("matter-rs pairing-info", START_TIMEOUT);
        let output = service.kill().join("\n");

        assert!(output.contains(READY), "{output}");
        for (name, secret) in [
            ("passcode", PASSCODE.to_string()),
            ("manual pairing code", manual_code.clone()),
            ("QR text", qr_text.clone()),
        ] {
            assert!(
                !output.contains(&secret),
                "{format}: the log contains the {name}"
            );
        }
        assert!(
            !output.contains("MT:"),
            "{format}: the log contains an onboarding payload"
        );
    }
}

#[test]
fn engine_logs_flow_through_tracing() {
    let directory = scratch("engine-logs");
    let config = write_config(&directory, 45_616, "json", "debug");
    let service = Service::start(&config);
    service.wait_for(READY, START_TIMEOUT);
    // An info line of rs-matter at start and a debug line once the mDNS responder announces.
    service.wait_for("Running Matter transport", START_TIMEOUT);
    service.wait_for("Broadcasting mDNS entry", START_TIMEOUT);
    let lines = service.kill();

    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| {
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("not a JSON line ({error}): {line}"))
        })
        .collect();
    let level_of = |target_prefix: &str, message_part: &str| {
        records
            .iter()
            .find(|record| {
                record["target"]
                    .as_str()
                    .is_some_and(|target| target.starts_with(target_prefix))
                    && record["fields"]["message"]
                        .as_str()
                        .is_some_and(|message| message.contains(message_part))
            })
            .map(|record| record["level"].as_str().unwrap_or_default().to_owned())
            .unwrap_or_else(|| {
                panic!(
                    "no record of {target_prefix} with {message_part:?}:\n{}",
                    lines.join("\n")
                )
            })
    };
    // rs-matter's own lines, written through the `log` facade, keep their target and level.
    assert_eq!(level_of("rs_matter", "Running Matter transport"), "INFO");
    // So do the engine implementation's.
    assert_eq!(
        level_of("matter_rs_engine_rsmatter", "node started"),
        "INFO"
    );
    // And the service's own.
    assert_eq!(level_of("matter_rs", READY), "INFO");
    // Debug lines of rs-matter pass the configured level.
    assert_eq!(level_of("rs_matter", "Broadcasting mDNS entry"), "DEBUG");
}

#[test]
fn run_help_describes_signals_and_exit_codes() {
    let (code, text) = run_once(matter_rs().args(["run", "--help"]));
    assert_eq!(code, Some(0), "{text}");
    for expected in [
        "SIGTERM",
        "SIGINT",
        "Ctrl-C",
        "second stop signal",
        "Exit codes: 0 after a clean stop",
        "1 when the node could not start",
        "RUST_LOG",
        "matter-rs pairing-info",
    ] {
        assert!(text.contains(expected), "{expected:?} missing:\n{text}");
    }
}
