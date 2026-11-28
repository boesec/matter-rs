//! A program under measurement as a console: lines in on standard input, lines out from
//! standard output and standard error, each wait with a deadline, and the line protocol of the
//! engine example on top.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// How long one protocol command may take to be answered.
pub(crate) const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

/// The input and output lines of a program.
pub(crate) struct Console {
    input: Option<Box<dyn Write + Send>>,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Console {
    /// A console on any writer and line channel; the tests feed it by hand.
    pub(crate) fn new(input: Box<dyn Write + Send>, lines: Receiver<String>) -> Self {
        Self {
            input: Some(input),
            lines,
            seen: Vec::new(),
        }
    }

    /// Sends one line.
    pub(crate) fn send(&mut self, line: &str) -> Result<(), String> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| String::from("standard input is closed"))?;
        writeln!(input, "{line}")
            .and_then(|()| input.flush())
            .map_err(|error| format!("sending {line:?}: {error}"))
    }

    /// The next line that satisfies `accept` within `timeout`; earlier lines are kept for the
    /// error message.
    pub(crate) fn wait_for(
        &mut self,
        what: &str,
        timeout: Duration,
        accept: impl Fn(&str) -> bool,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(remaining) {
                Ok(line) => {
                    let accepted = accept(&line);
                    self.seen.push(line.clone());
                    if accepted {
                        return Ok(line);
                    }
                }
                Err(reason) => {
                    let reason = match reason {
                        RecvTimeoutError::Timeout => format!("within {timeout:?}"),
                        RecvTimeoutError::Disconnected => String::from("before the output ended"),
                    };
                    let tail = self.seen.len().saturating_sub(40);
                    return Err(format!(
                        "no {what} {reason}; last output:\n{}",
                        self.seen[tail..].join("\n")
                    ));
                }
            }
        }
    }

    /// Sends a protocol command and returns its `ok` or the reason of its `err`.
    pub(crate) fn command(&mut self, line: &str) -> Result<(), String> {
        self.send(line)?;
        let answer = self.wait_for(&format!("answer to {line:?}"), REPLY_TIMEOUT, |l| {
            l == "ok" || l.starts_with("err ")
        })?;
        match answer.strip_prefix("err ") {
            Some(reason) => Err(format!("{line:?} failed: {reason}")),
            None => Ok(()),
        }
    }
}

/// Adds `count` devices of `kind` through the example's protocol, at `first` and the endpoints
/// after it, and waits for every `ok`. Returns once the last device is served.
pub(crate) fn add_devices(
    console: &mut Console,
    first: u16,
    count: u16,
    kind: &str,
) -> Result<u16, String> {
    let mut added = 0;
    for offset in 0..count {
        let endpoint = first
            .checked_add(offset)
            .ok_or_else(|| String::from("endpoint exceeds 16 bits"))?;
        console.command(&format!("add {endpoint} {kind}"))?;
        added += 1;
    }
    Ok(added)
}

/// Sends `quit` and reads the heap peak the example prints with `--heap-stats`.
pub(crate) fn quit_and_read_heap_peak(console: &mut Console) -> Result<u64, String> {
    console.command("quit")?;
    let line = console.wait_for("heap peak line", REPLY_TIMEOUT, |l| {
        l.starts_with("heap peak: ")
    })?;
    parse_heap_peak(&line)
}

/// The bytes of a `heap peak: <bytes>` line.
pub(crate) fn parse_heap_peak(line: &str) -> Result<u64, String> {
    line.strip_prefix("heap peak: ")
        .and_then(|value| value.trim().parse().ok())
        .ok_or_else(|| format!("{line:?} is not a heap peak line"))
}

/// A child process with its console; it is killed when dropped.
pub(crate) struct Process {
    child: Child,
    /// The console of the process.
    pub(crate) console: Console,
}

impl Process {
    /// Starts `command` with piped standard streams; standard output and standard error arrive
    /// on one channel, line by line.
    pub(crate) fn spawn(mut command: Command) -> Result<Self, String> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let program = format!("{:?}", command.get_program());
        let mut child = command
            .spawn()
            .map_err(|error| format!("starting {program}: {error}"))?;
        let (sender, lines) = mpsc::channel();
        if let Some(stdout) = child.stdout.take() {
            forward_lines(stdout, sender.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            forward_lines(stderr, sender);
        }
        let input = child
            .stdin
            .take()
            .ok_or_else(|| format!("{program}: no standard input"))?;
        Ok(Self {
            child,
            console: Console::new(Box::new(input), lines),
        })
    }

    /// The process id.
    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    /// Waits for the process to exit, at most `timeout`; kills it after that.
    pub(crate) fn wait_exit(&mut self, timeout: Duration) -> Result<ExitStatus, String> {
        self.console.input = None;
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return Err(format!("the process did not exit within {timeout:?}"));
                }
                Err(error) => return Err(format!("waiting for the process: {error}")),
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn forward_lines(stream: impl Read + Send + 'static, sender: Sender<String>) {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if sender.send(line).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use super::{Console, add_devices, parse_heap_peak, quit_and_read_heap_peak};

    /// A writer whose bytes the test reads back.
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<u8>>>);

    impl Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Recorder {
        fn text(&self) -> String {
            String::from_utf8(
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone(),
            )
            .expect("utf-8")
        }
    }

    fn console(output: &[&str]) -> (Console, Recorder) {
        let (sender, lines) = mpsc::channel();
        for line in output {
            sender.send((*line).to_owned()).expect("sent");
        }
        drop(sender);
        let recorder = Recorder::default();
        (Console::new(Box::new(recorder.clone()), lines), recorder)
    }

    #[test]
    fn devices_are_added_one_answer_at_a_time() {
        let (mut console, input) = console(&[
            "event CommissioningChanged commissioned=false window=Some(Basic)",
            "ok",
            "event something",
            "ok",
            "ok",
        ]);
        assert_eq!(add_devices(&mut console, 2, 3, "dimmable-light"), Ok(3));
        assert_eq!(
            input.text(),
            "add 2 dimmable-light\nadd 3 dimmable-light\nadd 4 dimmable-light\n"
        );
    }

    #[test]
    fn a_refused_device_stops_with_the_reason() {
        let (mut console, _) = console(&["ok", "err endpoint 3 exists"]);
        let error = add_devices(&mut console, 2, 5, "on-off-light").expect_err("refused");
        assert!(error.contains("endpoint 3 exists"), "{error}");
        assert!(error.contains("add 3 on-off-light"), "{error}");
    }

    #[test]
    fn a_missing_answer_is_an_error_with_the_output_so_far() {
        let (mut console, _) = console(&["ok", "event late"]);
        let error = add_devices(&mut console, 2, 2, "on-off-light").expect_err("no answer");
        assert!(
            error.contains("answer to \"add 3 on-off-light\""),
            "{error}"
        );
        assert!(error.contains("event late"), "{error}");
    }

    #[test]
    fn the_heap_peak_is_read_after_quit() {
        let (mut console, input) = console(&[
            "ok",
            "event Stopped Requested",
            "heap peak: 123456",
            "heap current: 1000",
        ]);
        assert_eq!(quit_and_read_heap_peak(&mut console), Ok(123_456));
        assert_eq!(input.text(), "quit\n");
        assert!(parse_heap_peak("heap peak: x").is_err());
        assert!(parse_heap_peak("heap current: 5").is_err());
    }

    #[test]
    fn waiting_times_out() {
        let (sender, lines) = mpsc::channel::<String>();
        let mut console = Console::new(Box::new(Recorder::default()), lines);
        let error = console
            .wait_for("ready line", Duration::from_millis(20), |_| true)
            .expect_err("nothing arrives");
        assert!(error.contains("within"), "{error}");
        drop(sender);
    }
}
