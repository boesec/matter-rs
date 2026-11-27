//! The log of the service: one `tracing` subscriber for the binary, the engine and rs-matter.
//!
//! The `[log]` section decides the output and the level:
//!
//! - `format = "text"` writes human-readable lines to standard error, coloured only when standard
//!   error is a terminal; `format = "json"` writes one JSON object per line instead.
//! - `journald = true` writes to the systemd journal (Linux, Cargo feature `journald`); on other
//!   platforms the configuration ignores the key with a warning and the output stays on standard
//!   error.
//! - `level` is the lowest level that is logged. A non-empty `RUST_LOG` replaces it with a full
//!   filter (`RUST_LOG=info,rs_matter=debug`), as `tracing-subscriber` documents.
//!
//! The engine and rs-matter write through the `log` facade; `tracing-log` turns their records
//! into events of the same subscriber, with their own target and level, so one filter and one
//! format cover everything.
//!
//! Nothing here formats a configuration value: the secrets of the configuration are
//! `Secret<T>`, whose `Debug` output is redacted, and the service never logs them.

use std::io::{self, IsTerminal as _};

use matter_rs_config::{LogFormat, LogLevel, LogSection};
use tracing::Dispatch;
use tracing::level_filters::LevelFilter;
use tracing_log::AsLog as _;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Layer, Layered, SubscriberExt as _};
use tracing_subscriber::{EnvFilter, Registry};

/// The environment variable whose filter replaces `[log].level`.
pub(crate) const FILTER_ENV: &str = "RUST_LOG";

/// Where the log goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Output {
    /// Text lines on standard error.
    Text,
    /// JSON lines on standard error.
    Json,
    /// The systemd journal.
    Journald,
}

impl Output {
    /// Whether the log reaches standard error, so that a start-up error need not be printed a
    /// second time.
    pub(crate) fn is_stderr(self) -> bool {
        !matches!(self, Self::Journald)
    }
}

/// The output the `[log]` section asks for on a platform with or without Linux and in a build
/// with or without the `journald` feature.
///
/// # Errors
///
/// A message when the journal is asked for on Linux but the build does not contain it.
pub(crate) fn select_output(
    log: &LogSection,
    linux: bool,
    journald_built: bool,
) -> Result<Output, String> {
    if log.journald && linux {
        return if journald_built {
            Ok(Output::Journald)
        } else {
            Err(String::from(
                "`log.journald` is set, but this binary was built without the Cargo feature \
                 `journald`",
            ))
        };
    }
    Ok(match log.format {
        LogFormat::Text => Output::Text,
        LogFormat::Json => Output::Json,
    })
}

/// The filter: `RUST_LOG` when it is set and not empty, `[log].level` otherwise.
///
/// # Errors
///
/// A message when `RUST_LOG` is not a valid filter.
pub(crate) fn filter(level: LogLevel, rust_log: Option<&str>) -> Result<EnvFilter, String> {
    match rust_log.map(str::trim).filter(|value| !value.is_empty()) {
        Some(directives) => EnvFilter::builder()
            .parse(directives)
            .map_err(|error| format!("{FILTER_ENV} is not a valid log filter: {error}")),
        None => Ok(EnvFilter::new(level_directive(level))),
    }
}

/// The filter directive of a configured level.
fn level_directive(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warn",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
        LogLevel::Trace => "trace",
    }
}

type Filtered = Layered<EnvFilter, Registry>;

/// Builds the subscriber for `output`, writing text and JSON to `writer`.
///
/// # Errors
///
/// A message when the journal cannot be reached.
pub(crate) fn dispatch<W>(
    output: Output,
    filter: EnvFilter,
    writer: W,
    ansi: bool,
) -> Result<Dispatch, String>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let layer: Box<dyn Layer<Filtered> + Send + Sync> = match output {
        Output::Text => tracing_subscriber::fmt::layer()
            .with_writer(writer)
            .with_ansi(ansi)
            .boxed(),
        Output::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_writer(writer)
            .with_ansi(false)
            .boxed(),
        Output::Journald => journald_layer()?,
    };
    Ok(Dispatch::new(
        tracing_subscriber::registry().with(filter).with(layer),
    ))
}

#[cfg(all(target_os = "linux", feature = "journald"))]
fn journald_layer() -> Result<Box<dyn Layer<Filtered> + Send + Sync>, String> {
    tracing_journald::layer()
        .map(|layer| {
            layer
                .with_syslog_identifier(String::from("matter-rs"))
                .boxed()
        })
        .map_err(|error| format!("cannot connect to the systemd journal: {error}"))
}

#[cfg(not(all(target_os = "linux", feature = "journald")))]
fn journald_layer() -> Result<Box<dyn Layer<Filtered> + Send + Sync>, String> {
    Err(String::from(
        "the systemd journal is not available in this build",
    ))
}

/// Installs the log of the service for the whole process and returns its output.
///
/// # Errors
///
/// A message when `RUST_LOG` is invalid, the journal is asked for but not available, or a
/// subscriber is installed already.
pub(crate) fn init(log: &LogSection) -> Result<Output, String> {
    let output = select_output(log, cfg!(target_os = "linux"), cfg!(feature = "journald"))?;
    let rust_log = std::env::var(FILTER_ENV).ok();
    let filter = filter(log.level, rust_log.as_deref())?;
    // The `log` macros of the engine and rs-matter test this level before they build a record,
    // so that records the filter would drop cost nothing.
    let max_level = filter.max_level_hint().unwrap_or(LevelFilter::TRACE);
    let ansi = output == Output::Text && io::stderr().is_terminal();
    let dispatch = dispatch(output, filter, io::stderr, ansi)?;
    tracing::dispatcher::set_global_default(dispatch)
        .map_err(|error| format!("cannot install the log: {error}"))?;
    tracing_log::LogTracer::builder()
        .with_max_level(max_level.as_log())
        .init()
        .map_err(|error| format!("cannot install the log bridge: {error}"))?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, Once, PoisonError};

    use matter_rs_config::{LogFormat, LogLevel, LogSection};

    use super::{Output, dispatch, filter, select_output};

    /// A writer that collects everything in memory.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner))
                .into_owned()
        }
    }

    impl io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn section(format: LogFormat, journald: bool) -> LogSection {
        LogSection {
            level: LogLevel::Info,
            format,
            journald,
        }
    }

    /// The `log` facade reaches the current subscriber only through the log tracer, which can
    /// be installed once per process.
    fn install_log_tracer() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            tracing_log::LogTracer::init().expect("no other logger in the unit tests");
        });
    }

    /// Runs `body` with a subscriber of `output` and `level` that writes to a buffer, and
    /// returns what it wrote.
    fn capture(output: Output, directives: &str, body: impl FnOnce()) -> String {
        install_log_tracer();
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let filter = filter(LogLevel::Info, Some(directives)).expect("a valid filter");
        let dispatch =
            dispatch(output, filter, move || writer.clone(), false).expect("a subscriber");
        tracing::dispatcher::with_default(&dispatch, body);
        buffer.text()
    }

    #[test]
    fn journald_layer_is_selected() {
        let journald = section(LogFormat::Json, true);
        assert_eq!(select_output(&journald, true, true), Ok(Output::Journald));
        let refused = select_output(&journald, true, false).expect_err("not built in");
        assert!(refused.contains("journald"), "{refused}");
        // Elsewhere the key is ignored (the configuration warns) and the format decides.
        assert_eq!(select_output(&journald, false, true), Ok(Output::Json));
        assert_eq!(
            select_output(&section(LogFormat::Text, false), true, true),
            Ok(Output::Text)
        );
        assert!(!Output::Journald.is_stderr());
        assert!(Output::Text.is_stderr() && Output::Json.is_stderr());

        // This build: the default feature set selects the journal on Linux.
        let this_build = select_output(
            &journald,
            cfg!(target_os = "linux"),
            cfg!(feature = "journald"),
        );
        if cfg!(all(target_os = "linux", feature = "journald")) {
            assert_eq!(this_build, Ok(Output::Journald));
        } else if !cfg!(target_os = "linux") {
            assert_eq!(this_build, Ok(Output::Json));
        }
    }

    /// Where a journal runs, the journald subscriber connects to it.
    #[cfg(all(target_os = "linux", feature = "journald"))]
    #[test]
    fn journald_layer_connects_where_a_journal_runs() {
        if !std::path::Path::new("/run/systemd/journal/socket").exists() {
            return;
        }
        let filter = filter(LogLevel::Info, None).unwrap();
        assert!(dispatch(Output::Journald, filter, io::sink, false).is_ok());
    }

    #[test]
    fn rust_log_replaces_the_level() {
        let filter_of = |level, rust_log| filter(level, rust_log).unwrap().to_string();
        assert_eq!(filter_of(LogLevel::Warn, None), "warn");
        assert_eq!(filter_of(LogLevel::Debug, Some("  ")), "debug");
        assert_eq!(
            filter_of(LogLevel::Warn, Some("info,rs_matter=trace")),
            "rs_matter=trace,info"
        );
        let invalid = filter(LogLevel::Info, Some("rs_matter=loud")).expect_err("invalid");
        assert!(invalid.contains("RUST_LOG"), "{invalid}");
    }

    #[test]
    fn log_records_keep_their_target_and_level() {
        let text = capture(Output::Json, "debug", || {
            log::info!(target: "rs_matter::transport", "transport running");
            log::debug!(target: "rs_matter::mdns", "query answered");
            log::trace!(target: "rs_matter::mdns", "filtered out");
            tracing::warn!("from the binary");
        });
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
            .collect();
        let find = |message: &str| {
            lines
                .iter()
                .find(|line| line["fields"]["message"] == message)
                .unwrap_or_else(|| panic!("{message:?} missing in {text}"))
        };
        let info = find("transport running");
        assert_eq!(info["level"], "INFO");
        assert_eq!(info["target"], "rs_matter::transport");
        let debug = find("query answered");
        assert_eq!(debug["level"], "DEBUG");
        assert_eq!(debug["target"], "rs_matter::mdns");
        assert_eq!(find("from the binary")["level"], "WARN");
        assert!(!text.contains("filtered out"), "{text}");
    }

    #[test]
    fn text_lines_name_level_and_target() {
        let text = capture(Output::Text, "info", || {
            log::warn!(target: "rs_matter::sc", "session evicted");
            log::debug!(target: "rs_matter::sc", "below the level");
        });
        let line = text
            .lines()
            .find(|line| line.contains("session evicted"))
            .unwrap_or_else(|| panic!("missing in {text}"));
        assert!(line.contains("WARN"), "{line}");
        assert!(line.contains("rs_matter::sc"), "{line}");
        assert!(!text.contains("below the level"), "{text}");
        assert!(
            !text.contains('\u{1b}'),
            "no colours without a terminal: {text:?}"
        );
    }
}
