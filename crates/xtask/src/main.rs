//! matter-rs-xtask: development tasks of the workspace (layer "tooling").
//!
//! Run as `cargo xtask <task>` through the alias in `.cargo/config.toml`. The crate depends on
//! no workspace crate: it builds and runs the workspace's binaries as processes, so that it
//! measures exactly what a user builds.
//!
//! # `cargo xtask footprint`
//!
//! Builds every build profile listed in `footprint.toml` with the Cargo profile
//! `release-small` and measures it on Linux:
//!
//! - the size of the stripped service binary, on the host and, with `--cross`, on the cross
//!   targets (built with `cargo zigbuild`);
//! - the resident set size of `matter-rs run` with the starter configuration and no devices;
//! - the engine example with bridged devices: its resident set size once the last device is
//!   served, and the heap peak its counting allocator reports.
//!
//! The numbers are compared with the budgets in `footprint.toml` and printed as a Markdown table
//! (also written to `--markdown <path>`, with the raw numbers in `--json <path>`). `--from-report
//! <report.json>` compares the numbers of an earlier run instead of measuring, on any host.
//! `--bless` writes the measured values plus a margin (`--margin`, 15 % by default) into
//! `footprint.toml`; it refuses to run when the `CI` environment variable is set.
//!
//! Exit codes: 0 when every measurement is within its budget (or after `--bless`), 1 when a
//! budget is exceeded or the measurement failed, 2 on wrong usage.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

mod config;
mod console;
mod measure;
mod report;

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use clap::{Args, Parser, Subcommand};

use crate::config::Footprint;
use crate::measure::{CARGO_PROFILE, Workspace, measure_all};
use crate::report::{Metric, Report, any_exceeded, compare, render_markdown};

/// Development tasks of the matter-rs workspace.
#[derive(Debug, Parser)]
#[command(name = "cargo xtask", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Build the profiles of footprint.toml, measure them on Linux and compare the numbers with
    /// the budgets; exit code 1 when a budget is exceeded.
    Footprint(FootprintArgs),
}

#[derive(Debug, Args)]
struct FootprintArgs {
    /// Also build the profiles for the cross targets (needs cargo-zigbuild and zig).
    #[arg(long)]
    cross: bool,
    /// Write the measured values plus the margin into the budget file. Refused in CI.
    #[arg(long)]
    bless: bool,
    /// The margin `--bless` adds, in percent.
    #[arg(long, default_value_t = 15, requires = "bless", value_name = "PERCENT")]
    margin: u32,
    /// The budget file [default: footprint.toml in the workspace root].
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
    /// Also write the Markdown report to this file (for a CI step summary).
    #[arg(long, value_name = "PATH")]
    markdown: Option<PathBuf>,
    /// Also write the measurements and their comparison as JSON to this file.
    #[arg(long, value_name = "PATH")]
    json: Option<PathBuf>,
    /// Compare the measurements of an earlier JSON report instead of measuring.
    #[arg(long, value_name = "REPORT")]
    from_report: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.task {
        Task::Footprint(args) => footprint(&args),
    };
    match outcome {
        Ok(Outcome::Passed) => ExitCode::SUCCESS,
        Ok(Outcome::Exceeded) => {
            eprintln!("footprint: a budget is exceeded");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!("footprint: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The result of a run that completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Exceeded,
}

/// The workspace root: this crate lives in `crates/xtask`.
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}

/// `--bless` is a person's decision: it refuses to run when `CI` is set to anything.
fn refuse_in_ci(ci: Option<&OsStr>) -> Result<(), String> {
    match ci {
        Some(value) if !value.is_empty() => Err(String::from(
            "--bless refuses to run in CI (the CI environment variable is set): budgets are set \
             by a person from a measurement, never by the pipeline that checks them",
        )),
        _ => Ok(()),
    }
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|error| format!("{}: {error}", path.display()))
}

/// What the memory rows of a subject describe, for the table.
fn notes(footprint: &Footprint) -> Vec<(String, String)> {
    let mut notes: Vec<(String, String)> = footprint
        .active_profiles()
        .map(|profile| (profile.name.clone(), String::from("idle")))
        .collect();
    notes.push((
        footprint.example.name.clone(),
        format!("{} devices", footprint.example.devices),
    ));
    notes
}

/// Every measurement a run produces: binary sizes of the profiles on the host and (with
/// `cross`) the cross targets, idle memory of the profiles, binary, memory and heap peak of
/// the example.
fn plan(footprint: &Footprint, cross: bool) -> Vec<(String, String, Metric)> {
    let host = &footprint.host_target;
    let example = &footprint.example.name;
    let mut plan = Vec::new();
    for profile in footprint.active_profiles() {
        for metric in [Metric::BinarySize, Metric::Rss, Metric::RssHighWater] {
            plan.push((profile.name.clone(), host.clone(), metric));
        }
        if cross {
            for target in &footprint.cross_targets {
                plan.push((profile.name.clone(), target.clone(), Metric::BinarySize));
            }
        }
    }
    for metric in [
        Metric::BinarySize,
        Metric::Rss,
        Metric::RssHighWater,
        Metric::HeapPeak,
    ] {
        plan.push((example.clone(), host.clone(), metric));
    }
    plan.sort();
    plan
}

/// Fails when a run did not produce exactly the planned measurements.
fn check_plan(footprint: &Footprint, report: &Report, cross: bool) -> Result<(), String> {
    let mut measured: Vec<(String, String, Metric)> = report
        .measurements
        .iter()
        .map(|m| (m.subject.clone(), m.target.clone(), m.metric))
        .collect();
    measured.sort();
    let expected = plan(footprint, cross);
    if measured == expected {
        Ok(())
    } else {
        Err(format!(
            "the run measured {measured:?}, the plan is {expected:?}"
        ))
    }
}

fn footprint(args: &FootprintArgs) -> Result<Outcome, String> {
    if args.bless {
        refuse_in_ci(std::env::var_os("CI").as_deref())?;
    }
    let root = workspace_root();
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| root.join("footprint.toml"));
    let mut footprint = Footprint::parse(&read(&config_path)?)
        .map_err(|error| format!("{}: {error}", config_path.display()))?;

    let report = match &args.from_report {
        Some(path) => serde_json::from_str::<Report>(&read(path)?)
            .map_err(|error| format!("{}: {error}", path.display()))?,
        None => {
            let started = Instant::now();
            let workspace = Workspace::locate(&root)?;
            eprintln!(
                "footprint: measuring with the Cargo profile {CARGO_PROFILE} in {}",
                workspace.target_dir.display()
            );
            let measurements = measure_all(&workspace, &footprint, args.cross)?;
            let report = Report {
                measurements,
                duration_seconds: started.elapsed().as_secs(),
                rows: Vec::new(),
            };
            check_plan(&footprint, &report, args.cross)?;
            report
        }
    };

    let rows = compare(&footprint, &report.measurements);
    let mut markdown = render_markdown(&rows, &notes(&footprint));
    if report.duration_seconds > 0 {
        markdown.push_str(&format!(
            "\nMeasured in {} s with the Cargo profile `{CARGO_PROFILE}`.\n",
            report.duration_seconds
        ));
    }
    print!("{markdown}");
    if let Some(path) = &args.markdown {
        write(path, &markdown)?;
    }
    if let Some(path) = &args.json {
        let full = Report {
            rows: rows.clone(),
            ..report.clone()
        };
        let text = serde_json::to_string_pretty(&full).map_err(|error| error.to_string())?;
        write(path, &text)?;
    }

    if args.bless {
        footprint.bless(&rows, args.margin);
        write(&config_path, &footprint.render())?;
        eprintln!(
            "footprint: wrote the measured values plus {} % into {}",
            args.margin,
            config_path.display()
        );
        return Ok(Outcome::Passed);
    }
    Ok(if any_exceeded(&rows) {
        Outcome::Exceeded
    } else {
        Outcome::Passed
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use clap::CommandFactory as _;

    use super::{Cli, plan, refuse_in_ci, workspace_root};
    use crate::config::{COMMITTED, Footprint};
    use crate::report::Metric;

    #[test]
    fn command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bless_refuses_to_run_in_ci() {
        let error = refuse_in_ci(Some(OsStr::new("true"))).expect_err("CI is set");
        assert!(error.contains("refuses to run in CI"), "{error}");
        assert!(refuse_in_ci(Some(OsStr::new("1"))).is_err());
        assert_eq!(refuse_in_ci(None), Ok(()));
        assert_eq!(refuse_in_ci(Some(OsStr::new(""))), Ok(()));
    }

    /// The committed budget file lists the profiles that were decided, with the feature sets
    /// that exist on the binary, every cross target of the toolchain, and a plan that measures
    /// every profile on every target.
    #[test]
    fn profile_matrix_is_complete() {
        let footprint = Footprint::parse(COMMITTED).expect("footprint.toml is valid");
        let profiles: Vec<(&str, bool, bool, Vec<&str>)> = footprint
            .profiles
            .iter()
            .map(|p| {
                (
                    p.name.as_str(),
                    p.planned,
                    p.default_features,
                    p.features.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            profiles,
            vec![
                ("minimal-bridge", false, false, vec!["role-bridge"]),
                ("full-bridge", false, true, vec![]),
                ("controller", true, true, vec![]),
            ]
        );

        // The features exist on the service binary.
        let manifest: toml::Table =
            std::fs::read_to_string(workspace_root().join("crates/matter-rs/Cargo.toml"))
                .expect("the binary's manifest")
                .parse()
                .expect("valid TOML");
        let features = manifest["features"].as_table().expect("[features]");
        for profile in footprint.active_profiles() {
            for feature in &profile.features {
                assert!(
                    features.contains_key(feature),
                    "{}: no feature {feature}",
                    profile.name
                );
            }
        }

        // The cross targets are installed with the pinned toolchain.
        let toolchain: toml::Table =
            std::fs::read_to_string(workspace_root().join("rust-toolchain.toml"))
                .expect("rust-toolchain.toml")
                .parse()
                .expect("valid TOML");
        let targets: Vec<&str> = toolchain["toolchain"]["targets"]
            .as_array()
            .expect("targets")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(footprint.host_target, "x86_64-unknown-linux-gnu");
        assert_eq!(
            footprint.cross_targets,
            [
                "aarch64-unknown-linux-musl",
                "armv7-unknown-linux-musleabihf"
            ]
        );
        for target in &footprint.cross_targets {
            assert!(
                targets.contains(&target.as_str()),
                "{target} is not in rust-toolchain.toml"
            );
        }

        // Every active profile is measured on every target; the example on the host.
        let plan = plan(&footprint, true);
        for profile in ["minimal-bridge", "full-bridge"] {
            for target in std::iter::once(&footprint.host_target).chain(&footprint.cross_targets) {
                assert!(
                    plan.contains(&(profile.to_owned(), target.clone(), Metric::BinarySize)),
                    "{profile} on {target}"
                );
            }
            assert!(plan.contains(&(
                profile.to_owned(),
                footprint.host_target.clone(),
                Metric::Rss
            )));
        }
        for metric in [Metric::Rss, Metric::HeapPeak] {
            assert!(plan.contains(&(
                String::from("onoff_node"),
                footprint.host_target.clone(),
                metric
            )));
        }
        assert!(plan.iter().all(|(subject, ..)| subject != "controller"));
        assert_eq!(plan.len(), 2 * 3 + 2 * 2 + 4);
        assert_eq!(super::plan(&footprint, false).len(), 2 * 3 + 4);
    }
}
