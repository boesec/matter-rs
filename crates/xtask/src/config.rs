//! The budget file `footprint.toml`: the build profiles, the targets, the engine example and
//! the budgets, how it is read and checked, and how `--bless` writes it back.
//!
//! The file is ours, so it is read strictly: unknown keys are refused, because a misspelt
//! budget key would otherwise silently turn a budget into "measure only". TOML has no `null`;
//! a budget key that is absent means "measure only", and the file shows it as a comment.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::Deserialize;

use crate::report::{Metric, Row};

/// The whole budget file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Footprint {
    /// The target the binaries are measured on, sizes and memory (the Linux runner).
    pub(crate) host_target: String,
    /// The cross targets whose binary sizes are measured with `--cross`.
    pub(crate) cross_targets: Vec<String>,
    /// The build profiles of the service binary, in the order of the report.
    #[serde(rename = "profile")]
    pub(crate) profiles: Vec<Profile>,
    /// The engine example that stands in for a bridge with devices.
    pub(crate) example: Example,
}

/// One build profile: a feature set of the service binary.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    /// The profile name, kebab-case.
    pub(crate) name: String,
    /// What the profile contains, in one line.
    pub(crate) description: String,
    /// A profile that is listed but not built yet.
    #[serde(default)]
    pub(crate) planned: bool,
    /// Whether the binary's default features are on.
    #[serde(default = "default_true")]
    pub(crate) default_features: bool,
    /// The features enabled on top.
    #[serde(default)]
    pub(crate) features: Vec<String>,
    /// The budgets by target.
    #[serde(default)]
    pub(crate) budget: BTreeMap<String, Budget>,
}

/// The budgets of one subject on one target; `None` is "measure only".
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Budget {
    /// The size of the stripped binary.
    pub(crate) max_binary_bytes: Option<u64>,
    /// The resident set size (`VmRSS`).
    pub(crate) max_rss_bytes: Option<u64>,
    /// The heap peak the counting allocator reports.
    pub(crate) max_heap_peak_bytes: Option<u64>,
}

/// The engine example with bridged devices: the stand-in for the service with devices until
/// the service has an adapter that feeds it devices.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Example {
    /// The package the example belongs to.
    pub(crate) package: String,
    /// The example's name.
    pub(crate) name: String,
    /// How many devices are added before the memory is read.
    pub(crate) devices: u16,
    /// The endpoint of the first device; the others follow without gaps.
    pub(crate) first_endpoint: u16,
    /// The device type of every device, as the example's `add` command names it.
    pub(crate) device_kind: String,
    /// The budgets on the host target.
    #[serde(default)]
    pub(crate) budget: Budget,
}

fn default_true() -> bool {
    true
}

impl Budget {
    /// The budget for `metric`; the peak RSS is never budgeted.
    pub(crate) fn get(&self, metric: Metric) -> Option<u64> {
        match metric {
            Metric::BinarySize => self.max_binary_bytes,
            Metric::Rss => self.max_rss_bytes,
            Metric::HeapPeak => self.max_heap_peak_bytes,
            Metric::RssHighWater => None,
        }
    }

    fn set(&mut self, metric: Metric, value: u64) {
        match metric {
            Metric::BinarySize => self.max_binary_bytes = Some(value),
            Metric::Rss => self.max_rss_bytes = Some(value),
            Metric::HeapPeak => self.max_heap_peak_bytes = Some(value),
            Metric::RssHighWater => {}
        }
    }
}

impl Footprint {
    /// Reads and checks the file's text.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let footprint: Self = toml::from_str(text).map_err(|error| error.to_string())?;
        footprint.check()?;
        Ok(footprint)
    }

    /// The profiles that are built and measured.
    pub(crate) fn active_profiles(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.iter().filter(|profile| !profile.planned)
    }

    /// The budget of a subject (a profile name or the example's name) on a target.
    pub(crate) fn budget(&self, subject: &str, target: &str) -> Option<&Budget> {
        if subject == self.example.name {
            return (target == self.host_target).then_some(&self.example.budget);
        }
        self.profiles
            .iter()
            .find(|profile| profile.name == subject)
            .and_then(|profile| profile.budget.get(target))
    }

    fn check(&self) -> Result<(), String> {
        if self.cross_targets.contains(&self.host_target) {
            return Err(format!(
                "{} is the host target and a cross target",
                self.host_target
            ));
        }
        let mut names: Vec<&str> = Vec::new();
        for profile in &self.profiles {
            let name = profile.name.as_str();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            {
                return Err(format!("profile name {name:?} is not kebab-case"));
            }
            if names.contains(&name) || name == self.example.name {
                return Err(format!("profile {name} is listed twice"));
            }
            names.push(name);
            if profile.planned && !profile.budget.is_empty() {
                return Err(format!("profile {name} is planned and cannot have budgets"));
            }
            for (target, budget) in &profile.budget {
                if *target != self.host_target && !self.cross_targets.contains(target) {
                    return Err(format!(
                        "profile {name}: {target} is neither the host target nor a cross target"
                    ));
                }
                if budget.max_heap_peak_bytes.is_some() {
                    return Err(format!(
                        "profile {name}: the service binary's heap peak is not measured yet; \
                         only the example has max_heap_peak_bytes"
                    ));
                }
                if *target != self.host_target && budget.max_rss_bytes.is_some() {
                    return Err(format!(
                        "profile {name}: {target} is a cross target, only its binary size is measured"
                    ));
                }
            }
        }
        let example = &self.example;
        if example.devices == 0 {
            return Err(String::from("example: devices must be at least 1"));
        }
        // Endpoint 0 is the root node and endpoint 1 the aggregator of the bridge.
        if example.first_endpoint < 2 {
            return Err(String::from("example: first_endpoint must be 2 or more"));
        }
        if example
            .first_endpoint
            .checked_add(example.devices - 1)
            .is_none()
        {
            return Err(String::from("example: the endpoints exceed 16 bits"));
        }
        Ok(())
    }

    /// Writes the budgets of every measured row: its value plus `margin_percent`, rounded up to
    /// a whole KiB. Budgets of what was not measured stay as they are; the peak RSS is never
    /// budgeted.
    pub(crate) fn bless(&mut self, rows: &[Row], margin_percent: u32) {
        for row in rows {
            let Some(value) = row.value else { continue };
            if row.metric == Metric::RssHighWater {
                continue;
            }
            let budget = with_margin(value, margin_percent);
            if row.subject == self.example.name {
                if row.target == self.host_target {
                    self.example.budget.set(row.metric, budget);
                }
                continue;
            }
            if let Some(profile) = self
                .profiles
                .iter_mut()
                .find(|profile| profile.name == row.subject && !profile.planned)
            {
                profile
                    .budget
                    .entry(row.target.clone())
                    .or_default()
                    .set(row.metric, budget);
            }
        }
    }

    /// The file's text in its one canonical layout, with the explaining comments.
    pub(crate) fn render(&self) -> String {
        let mut out = String::from(HEADER);
        let _ = writeln!(out, "host_target = {}", quoted(&self.host_target));
        let _ = writeln!(out, "cross_targets = {}", string_array(&self.cross_targets));
        for profile in &self.profiles {
            out.push('\n');
            out.push_str("[[profile]]\n");
            let _ = writeln!(out, "name = {}", quoted(&profile.name));
            let _ = writeln!(out, "description = {}", quoted(&profile.description));
            if profile.planned {
                out.push_str("planned = true\n");
                continue;
            }
            let _ = writeln!(out, "default_features = {}", profile.default_features);
            let _ = writeln!(out, "features = {}", string_array(&profile.features));
            let empty = Budget::default();
            let host = profile.budget.get(&self.host_target).unwrap_or(&empty);
            out.push('\n');
            out.push_str(HOST_BUDGET_COMMENT);
            let _ = writeln!(out, "[profile.budget.{}]", self.host_target);
            render_key(&mut out, "max_binary_bytes", host.max_binary_bytes);
            render_key(&mut out, "max_rss_bytes", host.max_rss_bytes);
            for target in &self.cross_targets {
                let cross = profile.budget.get(target).unwrap_or(&empty);
                out.push('\n');
                let _ = writeln!(out, "[profile.budget.{target}]");
                render_key(&mut out, "max_binary_bytes", cross.max_binary_bytes);
            }
        }
        let example = &self.example;
        out.push('\n');
        out.push_str(EXAMPLE_COMMENT);
        out.push_str("[example]\n");
        let _ = writeln!(out, "package = {}", quoted(&example.package));
        let _ = writeln!(out, "name = {}", quoted(&example.name));
        let _ = writeln!(out, "devices = {}", example.devices);
        let _ = writeln!(out, "first_endpoint = {}", example.first_endpoint);
        let _ = writeln!(out, "device_kind = {}", quoted(&example.device_kind));
        out.push('\n');
        out.push_str(EXAMPLE_BUDGET_COMMENT);
        out.push_str("[example.budget]\n");
        render_key(
            &mut out,
            "max_binary_bytes",
            example.budget.max_binary_bytes,
        );
        render_key(&mut out, "max_rss_bytes", example.budget.max_rss_bytes);
        render_key(
            &mut out,
            "max_heap_peak_bytes",
            example.budget.max_heap_peak_bytes,
        );
        out
    }
}

const HEADER: &str = "\
# Footprint budgets of the matter-rs build profiles. All values are bytes.
#
# `cargo xtask footprint` builds every profile with the Cargo profile `release-small` (stripped,
# fat LTO, one codegen unit), measures it on Linux and compares the numbers with the budgets
# below; a measurement above its budget fails the run and the footprint workflow. A budget key
# that is commented out means \"measure only\" (TOML has no null).
#
# `cargo xtask footprint --bless` writes the measured values plus a margin (15 % by default)
# into this file. It rewrites the whole file in this layout, so edit the values only. It refuses
# to run in CI, where the budgets are checked, never changed.

";

const HOST_BUDGET_COMMENT: &str = "\
# The stripped binary, and the resident set size of `matter-rs run` with the starter
# configuration and no devices, a few seconds after the node is running. Cross targets: the
# stripped binary only.
";

const EXAMPLE_COMMENT: &str = "\
# The engine example with bridged devices, the stand-in for a bridge with devices until the
# service has an adapter that feeds it devices. It is built with `release-small`, started with
# `--heap-stats`, gets its devices through its line protocol on standard input, and is measured
# once the last device is served: the resident set size from /proc, then the heap peak it prints
# on `quit`.
";

const EXAMPLE_BUDGET_COMMENT: &str = "\
# Budgets on the host target.
";

fn render_key(out: &mut String, key: &str, value: Option<u64>) {
    match value {
        Some(value) => {
            let _ = writeln!(out, "{key} = {}", grouped(value));
        }
        None => {
            let _ = writeln!(out, "# {key} = (measure only)");
        }
    }
}

fn quoted(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn string_array(items: &[String]) -> String {
    let items: Vec<String> = items.iter().map(|item| quoted(item)).collect();
    format!("[{}]", items.join(", "))
}

/// An integer with `_` between groups of three digits, as TOML allows.
pub(crate) fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push('_');
        }
        out.push(digit);
    }
    out
}

/// `value` plus `margin_percent`, rounded up to a whole KiB.
pub(crate) fn with_margin(value: u64, margin_percent: u32) -> u64 {
    const KIB: u128 = 1024;
    let raised = (u128::from(value) * (100 + u128::from(margin_percent))).div_ceil(100);
    let rounded = raised.div_ceil(KIB) * KIB;
    u64::try_from(rounded).unwrap_or(u64::MAX)
}

/// The committed budget file of the workspace.
#[cfg(test)]
pub(crate) const COMMITTED: &str = include_str!("../../../footprint.toml");

#[cfg(test)]
mod tests {
    use super::{COMMITTED, Footprint, grouped, with_margin};
    use crate::report::{Metric, Row, Verdict};

    /// A budget file with budgets on nearly every measurement, for the comparison tests.
    const FIXTURE: &str = include_str!("../fixtures/footprint.toml");

    fn row(subject: &str, target: &str, metric: Metric, value: Option<u64>) -> Row {
        Row {
            subject: subject.to_owned(),
            target: target.to_owned(),
            metric,
            value,
            budget: None,
            verdict: Verdict::MeasureOnly,
        }
    }

    #[test]
    fn committed_file_is_valid_and_canonical() {
        let footprint = Footprint::parse(COMMITTED).expect("footprint.toml is valid");
        assert_eq!(
            footprint.render(),
            COMMITTED,
            "footprint.toml is not in the layout `--bless` writes"
        );
    }

    #[test]
    fn fixture_round_trips_through_render() {
        let footprint = Footprint::parse(FIXTURE).expect("the fixture is valid");
        assert_eq!(Footprint::parse(&footprint.render()), Ok(footprint));
    }

    #[test]
    fn invalid_files_are_refused_with_a_reason() {
        let cases: [(&str, &str, &str); 9] = [
            (
                "max_binary_bytes = 4_000_000",
                "max_binarry_bytes = 4_000_000",
                "unknown field",
            ),
            (
                "name = \"full-bridge\"",
                "name = \"minimal-bridge\"",
                "listed twice",
            ),
            (
                "name = \"full-bridge\"",
                "name = \"Full Bridge\"",
                "kebab-case",
            ),
            ("devices = 20", "devices = 0", "at least 1"),
            ("first_endpoint = 2", "first_endpoint = 1", "2 or more"),
            (
                "[profile.budget.aarch64-unknown-linux-musl]\n",
                "[profile.budget.aarch64-unknown-linux-musl]\nmax_rss_bytes = 1\n",
                "only its binary size",
            ),
            (
                "max_rss_bytes = 12_000_000\n\n[profile.budget.aarch64",
                "max_rss_bytes = 12_000_000\nmax_heap_peak_bytes = 1\n\n[profile.budget.aarch64",
                "heap peak is not measured",
            ),
            (
                "[[profile]]
name = \"full-bridge\"",
                "[profile.budget.riscv64gc-unknown-linux-musl]

[[profile]]
name = \"full-bridge\"",
                "neither the host target",
            ),
            (
                "planned = true
",
                "planned = true

[profile.budget.x86_64-unknown-linux-gnu]
",
                "planned and cannot have budgets",
            ),
        ];
        for (from, to, expected) in cases {
            assert!(FIXTURE.contains(from), "the fixture contains {from:?}");
            let text = FIXTURE.replacen(from, to, 1);
            let error = Footprint::parse(&text).expect_err(expected);
            assert!(error.contains(expected), "{expected:?} not in: {error}");
        }
    }

    #[test]
    fn margin_is_added_and_rounded_up_to_a_kib() {
        assert_eq!(with_margin(1_000_000, 15), 1_150_976);
        assert_eq!(with_margin(1024, 0), 1024);
        assert_eq!(with_margin(1025, 0), 2048);
        assert_eq!(with_margin(0, 15), 0);
        assert_eq!(with_margin(u64::MAX, 15), u64::MAX);
    }

    #[test]
    fn bless_writes_measured_values_with_margin_and_keeps_the_rest() {
        let mut footprint = Footprint::parse(FIXTURE).expect("the fixture is valid");
        let host = footprint.host_target.clone();
        let cross = footprint.cross_targets[0].clone();
        let untouched = footprint.profiles[1].budget[&cross].clone();
        footprint.bless(
            &[
                row("minimal-bridge", &host, Metric::BinarySize, Some(1_000_000)),
                row("minimal-bridge", &host, Metric::RssHighWater, Some(9)),
                row(
                    "minimal-bridge",
                    &cross,
                    Metric::BinarySize,
                    Some(2_000_000),
                ),
                row("full-bridge", &cross, Metric::BinarySize, None),
                row("onoff_node", &host, Metric::HeapPeak, Some(3_000_000)),
                row("controller", &host, Metric::BinarySize, Some(5)),
            ],
            15,
        );
        let minimal = &footprint.profiles[0];
        assert_eq!(minimal.budget[&host].max_binary_bytes, Some(1_150_976));
        assert_eq!(minimal.budget[&host].max_rss_bytes, Some(12_000_000));
        assert_eq!(minimal.budget[&cross].max_binary_bytes, Some(2_300_928));
        assert_eq!(footprint.profiles[1].budget[&cross], untouched);
        assert_eq!(
            footprint.example.budget.max_heap_peak_bytes,
            Some(3_450_880)
        );
        assert!(footprint.profiles[2].planned);
        assert!(footprint.profiles[2].budget.is_empty());
        // The result is a valid file again.
        assert_eq!(Footprint::parse(&footprint.render()), Ok(footprint));
    }

    #[test]
    fn integers_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1_000");
        assert_eq!(grouped(4_194_304), "4_194_304");
        assert_eq!(grouped(123_456_789_012), "123_456_789_012");
    }
}
