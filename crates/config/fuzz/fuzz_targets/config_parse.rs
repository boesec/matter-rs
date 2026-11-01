//! Feeds arbitrary input to the configuration loader. Loading must return a configuration or a
//! report for every input and never panic.
//!
//! Run with `cargo fuzz run config_parse` from `crates/config` (nightly toolchain, Linux).
//!
//! The crate cannot forbid unsafe code like the workspace crates do: the `fuzz_target!` macro
//! expands to the `#[unsafe(no_mangle)]` entry point that libFuzzer calls.

#![no_main]

use std::io;
use std::path::Path;

use libfuzzer_sys::fuzz_target;
use matter_rs_config::{ConfigSource, Environment, Platform, load_source};

/// An environment with only `HOME` set and no files, so that the input alone decides the outcome
/// and the target never touches the real system.
struct FuzzEnvironment;

impl Environment for FuzzEnvironment {
    fn var(&self, name: &str) -> Option<String> {
        // `HOME` lets the default state directory resolve, so valid inputs load completely.
        (name == "HOME").then(|| String::from("/home/fuzz"))
    }

    fn read_file(&self, _path: &Path) -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no files while fuzzing",
        ))
    }

    fn platform(&self) -> Platform {
        Platform::Unix
    }

    fn is_privileged(&self) -> bool {
        false
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let source = ConfigSource {
        name: "fuzz.toml",
        text,
        directory: None,
        mode: Some(0o644),
    };
    let _ = load_source(&source, &FuzzEnvironment);
});
