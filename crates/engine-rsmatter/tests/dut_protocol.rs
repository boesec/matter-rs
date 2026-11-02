//! Drives the built `onoff_node` example through its line protocol on standard input, the
//! way the interop harness does: every command is answered, events are printed, and no
//! secret appears until it is asked for.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

/// The passcode and discriminator the node is started with, and the manual pairing code
/// they produce (the development test vector).
const PASSCODE: &str = "20202021";
const DISCRIMINATOR: &str = "3840";
const MANUAL_CODE: &str = "34970112332";

const READY_TIMEOUT: Duration = Duration::from_secs(60);
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// The example binary next to this test's binary.
///
/// `cargo test` builds the example as a test harness (its parser has unit tests) but does not
/// refresh the plain binary, so the test asks cargo for it; that is cheap when it is up to date.
fn example_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("the test binary path");
    // target/<profile>/deps/<test> -> target/<profile>/examples/onoff_node
    path.pop();
    path.pop();
    let profile_dir = path.clone();
    path.push("examples");
    path.push(format!("onoff_node{}", std::env::consts::EXE_SUFFIX));
    let mut build = Command::new(env!("CARGO"));
    build
        .args(["build", "--example", "onoff_node", "--manifest-path"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .arg("--target-dir")
        .arg(profile_dir.parent().expect("the target directory"));
    if !cfg!(debug_assertions) {
        build.arg("--release");
    }
    let status = build.status().expect("cargo runs");
    assert!(status.success(), "the example builds");
    assert!(path.is_file(), "example binary at {}", path.display());
    path
}

/// The running example with its output lines on a channel.
struct Dut {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Dut {
    fn start(state_dir: &std::path::Path) -> Self {
        let mut command = Command::new(example_binary());
        command
            .arg("--state-dir")
            .arg(state_dir)
            .args(["--port", "0"])
            .args(["--discriminator", DISCRIMINATOR])
            .args(["--passcode", PASSCODE])
            .arg("--no-initial-device")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(interface) = std::env::var("MATTER_RS_TEST_INTERFACE")
            .ok()
            .filter(|name| !name.is_empty())
        {
            command.args(["--interface", &interface]);
        }
        let mut child = command.spawn().expect("the example starts");
        let stdout = child.stdout.take().expect("piped stdout");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let child_stderr = child.stderr.take().expect("piped stderr");
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
            for line in BufReader::new(child_stderr).lines() {
                let Ok(line) = line else { break };
                sink.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        Self {
            child,
            lines,
            seen: Vec::new(),
            stderr,
        }
    }

    /// The next line that satisfies `accept`, within `timeout`; earlier lines are kept.
    fn expect(&mut self, what: &str, timeout: Duration, accept: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    self.seen.push(line.clone());
                    if accept(&line) {
                        return line;
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {
                    let stderr = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
                    panic!(
                        "no {what} within {timeout:?}; output so far:\n{}\nstandard error:\n{}",
                        self.seen.join("\n"),
                        stderr.join("\n")
                    );
                }
            }
        }
    }

    fn send(&mut self, line: &str) {
        let stdin = self.child.stdin.as_mut().expect("piped stdin");
        writeln!(stdin, "{line}").expect("the example reads its input");
        stdin.flush().expect("flushed");
    }

    /// Sends a command and returns its `ok` or `err ...` answer.
    fn command(&mut self, line: &str) -> String {
        self.send(line);
        self.expect(&format!("answer to {line:?}"), REPLY_TIMEOUT, |l| {
            l == "ok" || l.starts_with("err ")
        })
    }
}

impl Drop for Dut {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn dut_protocol_round_trip() {
    let state_dir =
        std::env::temp_dir().join(format!("matter-rs-dut-protocol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    let mut dut = Dut::start(&state_dir);
    let ready = dut.expect("ready line", READY_TIMEOUT, |l| {
        l.starts_with("onoff_node: ready")
    });
    assert!(ready.contains("commissioned=false"), "{ready}");
    assert!(ready.contains("window=Some(Basic)"), "{ready}");

    // Endpoints of every device type, one composed under another.
    assert_eq!(dut.command("add 2 on-off-light"), "ok");
    assert_eq!(dut.command("add 3 dimmable-light"), "ok");
    assert_eq!(dut.command("add 4 contact-sensor"), "ok");
    assert_eq!(dut.command("add 5 temperature-sensor 2"), "ok");
    let duplicate = dut.command("add 2 on-off-light");
    assert!(
        duplicate.starts_with("err ") && duplicate.contains("exists already"),
        "{duplicate}"
    );
    let unknown_type = dut.command("add 6 toaster");
    assert!(
        unknown_type.contains("unknown device type"),
        "{unknown_type}"
    );
    let unknown_parent = dut.command("add 6 contact-sensor 99");
    assert!(
        unknown_parent.contains("unknown endpoint 99"),
        "{unknown_parent}"
    );

    // Values in the kind of their attribute, refusals with a reason.
    assert_eq!(dut.command("set 2 0x0006 0x0000 true"), "ok");
    assert_eq!(dut.command("set 3 0x0008 0x0000 128"), "ok");
    assert_eq!(dut.command("set 3 0x0008 0x0011 null"), "ok");
    assert_eq!(dut.command("set 4 0x0045 0x0000 true"), "ok");
    assert_eq!(dut.command("set 5 0x0402 0x0000 -550"), "ok");
    assert_eq!(dut.command("set 5 0x0402 0x0000 null"), "ok");
    assert_eq!(dut.command("set 2 0x0039 0x0005 Living room"), "ok");
    let wrong_kind = dut.command("set 2 0x0006 0x0000 7");
    assert!(wrong_kind.contains("not a bool"), "{wrong_kind}");
    let not_nullable = dut.command("set 2 0x0006 0x0000 null");
    assert!(not_nullable.contains("not nullable"), "{not_nullable}");
    let unknown_attribute = dut.command("set 2 0x0006 0x4000 true");
    assert!(
        unknown_attribute.contains("has no attribute"),
        "{unknown_attribute}"
    );
    let unknown_endpoint = dut.command("set 9 0x0006 0x0000 true");
    assert!(
        unknown_endpoint.contains("unknown endpoint 9"),
        "{unknown_endpoint}"
    );

    assert_eq!(dut.command("reachable 4 false"), "ok");
    assert_eq!(dut.command("reachable 4 true"), "ok");
    let bad_flag = dut.command("reachable 4 maybe");
    assert!(bad_flag.contains("not true or false"), "{bad_flag}");

    // Removing the light takes the sensor composed under it along.
    assert_eq!(dut.command("remove 2"), "ok");
    let gone = dut.command("set 5 0x0402 0x0000 100");
    assert!(gone.contains("unknown endpoint 5"), "{gone}");
    let gone = dut.command("remove 5");
    assert!(gone.starts_with("err "), "{gone}");
    assert_eq!(dut.command("add 2 on-off-light"), "ok");

    let garbage = dut.command("fly 2");
    assert!(garbage.contains("unknown command"), "{garbage}");

    // Nothing so far showed a secret.
    for line in &dut.seen {
        assert!(!line.contains(PASSCODE), "passcode printed: {line}");
        assert!(!line.contains(MANUAL_CODE), "pairing code printed: {line}");
        assert!(!line.contains("MT:"), "QR text printed: {line}");
    }

    // On request the codes are printed, then the answer.
    dut.send("pairing");
    let code = dut.expect("pairing code line", REPLY_TIMEOUT, |l| {
        l.starts_with("onoff_node: pairing code ")
    });
    assert_eq!(code, format!("onoff_node: pairing code {MANUAL_CODE}"));
    let qr = dut.expect("QR text line", REPLY_TIMEOUT, |l| {
        l.starts_with("onoff_node: qr text ")
    });
    assert!(qr.contains("MT:"), "{qr}");
    assert_eq!(
        dut.expect("answer to pairing", REPLY_TIMEOUT, |l| l == "ok"
            || l.starts_with("err ")),
        "ok"
    );

    // `quit` stops the node: the answer and the stop event are printed (in either order, the
    // event loop and the command thread run side by side) and the process exits cleanly.
    dut.send("quit");
    let mut answers = Vec::new();
    for _ in 0..2 {
        answers.push(dut.expect("answer or stop event", REPLY_TIMEOUT, |l| {
            l == "ok" || l == "event Stopped Requested"
        }));
    }
    answers.sort();
    assert_eq!(answers, ["event Stopped Requested", "ok"]);
    let status = dut.child.wait().expect("the process ends");
    assert!(status.success(), "exit status {status}");
    let _ = std::fs::remove_dir_all(&state_dir);
}
