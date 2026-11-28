//! The measurements, their comparison with the budgets, and the report: a Markdown table for
//! the terminal and the step summary, and JSON for the artefact and `--from-report`.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::config::Footprint;

/// What is measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Metric {
    /// The size of the stripped binary.
    BinarySize,
    /// The resident set size (`VmRSS`) at the moment of measurement.
    Rss,
    /// The peak resident set size so far (`VmHWM`); shown, never budgeted.
    RssHighWater,
    /// The heap peak the counting allocator reports.
    HeapPeak,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Self::BinarySize => "stripped binary",
            Self::Rss => "RSS",
            Self::RssHighWater => "peak RSS (VmHWM)",
            Self::HeapPeak => "heap peak",
        }
    }
}

/// One measured value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Measurement {
    /// A profile name, or the example's name.
    pub(crate) subject: String,
    /// The target triple the value was measured for.
    pub(crate) target: String,
    /// What was measured.
    pub(crate) metric: Metric,
    /// The value in bytes.
    pub(crate) value: u64,
}

/// The outcome of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Verdict {
    /// Measured and within its budget.
    Within,
    /// Measured and above its budget: the run fails.
    Exceeded,
    /// Measured, without a budget.
    MeasureOnly,
    /// Budgeted but not measured in this run (for example the cross targets without
    /// `--cross`).
    NotMeasured,
}

/// One row of the report: a measurement with its budget, or a budget without a measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Row {
    /// A profile name, or the example's name.
    pub(crate) subject: String,
    /// The target triple.
    pub(crate) target: String,
    /// What was measured.
    pub(crate) metric: Metric,
    /// The measured value, absent when it was not measured in this run.
    pub(crate) value: Option<u64>,
    /// The budget, absent for "measure only".
    pub(crate) budget: Option<u64>,
    /// The outcome.
    pub(crate) verdict: Verdict,
}

/// A complete report as the JSON artefact holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Report {
    /// The raw measurements of the run.
    pub(crate) measurements: Vec<Measurement>,
    /// How long the run took, in seconds.
    #[serde(default)]
    pub(crate) duration_seconds: u64,
    /// The comparison with the budgets at the time of the run; recomputed when a report is
    /// read back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) rows: Vec<Row>,
}

/// Compares the measurements with the budgets: one row per measurement, in the order of the
/// measurements, then one row per budget nothing was measured for.
pub(crate) fn compare(footprint: &Footprint, measurements: &[Measurement]) -> Vec<Row> {
    let mut rows: Vec<Row> = measurements
        .iter()
        .map(|measurement| {
            let budget = footprint
                .budget(&measurement.subject, &measurement.target)
                .and_then(|budget| budget.get(measurement.metric));
            let verdict = match budget {
                None => Verdict::MeasureOnly,
                Some(budget) if measurement.value > budget => Verdict::Exceeded,
                Some(_) => Verdict::Within,
            };
            Row {
                subject: measurement.subject.clone(),
                target: measurement.target.clone(),
                metric: measurement.metric,
                value: Some(measurement.value),
                budget,
                verdict,
            }
        })
        .collect();

    let mut budgeted: Vec<(String, String, Metric, u64)> = Vec::new();
    for profile in footprint.active_profiles() {
        for (target, budget) in &profile.budget {
            for metric in [Metric::BinarySize, Metric::Rss, Metric::HeapPeak] {
                if let Some(value) = budget.get(metric) {
                    budgeted.push((profile.name.clone(), target.clone(), metric, value));
                }
            }
        }
    }
    for metric in [Metric::BinarySize, Metric::Rss, Metric::HeapPeak] {
        if let Some(value) = footprint.example.budget.get(metric) {
            budgeted.push((
                footprint.example.name.clone(),
                footprint.host_target.clone(),
                metric,
                value,
            ));
        }
    }
    for (subject, target, metric, budget) in budgeted {
        let measured = rows
            .iter()
            .any(|row| row.subject == subject && row.target == target && row.metric == metric);
        if !measured {
            rows.push(Row {
                subject,
                target,
                metric,
                value: None,
                budget: Some(budget),
                verdict: Verdict::NotMeasured,
            });
        }
    }
    rows
}

/// Whether any row exceeds its budget.
pub(crate) fn any_exceeded(rows: &[Row]) -> bool {
    rows.iter().any(|row| row.verdict == Verdict::Exceeded)
}

/// The report as Markdown: a summary line and the table.
pub(crate) fn render_markdown(rows: &[Row], notes: &[(String, String)]) -> String {
    let count = |verdict: Verdict| rows.iter().filter(|row| row.verdict == verdict).count();
    let mut out = String::from("## Footprint\n\n");
    let exceeded = count(Verdict::Exceeded);
    let _ = writeln!(
        out,
        "{}: {} within budget, {} exceeded, {} measured only, {} not measured.\n",
        if exceeded == 0 {
            "Passed"
        } else {
            "**Failed**"
        },
        count(Verdict::Within),
        exceeded,
        count(Verdict::MeasureOnly),
        count(Verdict::NotMeasured),
    );
    out.push_str("| Subject | Target | Measurement | Value | Budget | Result |\n");
    out.push_str("|---|---|---|---|---|---|\n");
    for row in rows {
        let note = notes
            .iter()
            .find(|(subject, _)| *subject == row.subject)
            .map(|(_, note)| note.as_str());
        let measurement = match (row.metric, note) {
            (Metric::Rss | Metric::RssHighWater | Metric::HeapPeak, Some(note)) => {
                format!("{} ({note})", row.metric.label())
            }
            _ => row.metric.label().to_owned(),
        };
        let value = row.value.map_or_else(|| String::from("–"), bytes);
        let budget = row.budget.map_or_else(|| String::from("–"), bytes);
        let result = match (row.verdict, row.value, row.budget) {
            (Verdict::Within, Some(value), Some(budget)) => {
                format!("ok ({} %)", percent(value, budget))
            }
            (Verdict::Exceeded, Some(value), Some(budget)) => {
                format!("**exceeded** ({} %)", percent(value, budget))
            }
            (Verdict::NotMeasured, ..) => String::from("not measured"),
            _ => String::from("measured only"),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {measurement} | {value} | {budget} | {result} |",
            row.subject, row.target
        );
    }
    out
}

/// Bytes as `1.23 MiB (1_289_748)`, with integer arithmetic.
pub(crate) fn bytes(value: u64) -> String {
    const MIB: u128 = 1024 * 1024;
    let hundredths = (u128::from(value) * 100 + MIB / 2) / MIB;
    format!(
        "{}.{:02} MiB ({})",
        hundredths / 100,
        hundredths % 100,
        crate::config::grouped(value)
    )
}

/// `value` as a share of `budget` in whole percent, rounded down.
fn percent(value: u64, budget: u64) -> u128 {
    if budget == 0 {
        return if value == 0 { 100 } else { u128::MAX };
    }
    u128::from(value) * 100 / u128::from(budget)
}

#[cfg(test)]
mod tests {
    use super::{Metric, Report, Row, Verdict, any_exceeded, bytes, compare, render_markdown};
    use crate::config::Footprint;

    /// A report of a run with `--cross`, every value below the fixture budgets except the
    /// full bridge's `x86_64` binary.
    const REPORT: &str = include_str!("../fixtures/report.json");

    fn fixture() -> (Footprint, Report) {
        let footprint = Footprint::parse(include_str!("../fixtures/footprint.toml"))
            .expect("the fixture budgets are valid");
        let report: Report = serde_json::from_str(REPORT).expect("the fixture report parses");
        (footprint, report)
    }

    #[test]
    fn budget_comparison() {
        let (footprint, report) = fixture();
        let rows = compare(&footprint, &report.measurements);
        let verdicts: Vec<(&str, &str, Metric, Verdict)> = rows
            .iter()
            .map(|row| {
                (
                    row.subject.as_str(),
                    row.target.as_str(),
                    row.metric,
                    row.verdict,
                )
            })
            .collect();
        let host = "x86_64-unknown-linux-gnu";
        let arm64 = "aarch64-unknown-linux-musl";
        let arm32 = "armv7-unknown-linux-musleabihf";
        assert_eq!(
            verdicts,
            vec![
                ("minimal-bridge", host, Metric::BinarySize, Verdict::Within),
                ("full-bridge", host, Metric::BinarySize, Verdict::Exceeded),
                ("onoff_node", host, Metric::BinarySize, Verdict::MeasureOnly),
                ("minimal-bridge", host, Metric::Rss, Verdict::Within),
                (
                    "minimal-bridge",
                    host,
                    Metric::RssHighWater,
                    Verdict::MeasureOnly
                ),
                ("full-bridge", host, Metric::Rss, Verdict::Within),
                (
                    "full-bridge",
                    host,
                    Metric::RssHighWater,
                    Verdict::MeasureOnly
                ),
                ("onoff_node", host, Metric::Rss, Verdict::Within),
                (
                    "onoff_node",
                    host,
                    Metric::RssHighWater,
                    Verdict::MeasureOnly
                ),
                ("onoff_node", host, Metric::HeapPeak, Verdict::Within),
                ("minimal-bridge", arm64, Metric::BinarySize, Verdict::Within),
                ("full-bridge", arm64, Metric::BinarySize, Verdict::Within),
                ("minimal-bridge", arm32, Metric::BinarySize, Verdict::Within),
                // The fixture has a budget for this one but the report has no value.
                (
                    "full-bridge",
                    arm32,
                    Metric::BinarySize,
                    Verdict::NotMeasured
                ),
            ]
        );
        assert!(any_exceeded(&rows));

        // A value equal to its budget is within it; one byte more exceeds it.
        let mut measurements = report.measurements.clone();
        measurements[1].value = 5_000_000;
        assert!(!any_exceeded(&compare(&footprint, &measurements)));
        measurements[1].value = 5_000_001;
        assert!(any_exceeded(&compare(&footprint, &measurements)));
    }

    #[test]
    fn measure_only_never_fails() {
        let (mut footprint, report) = fixture();
        for profile in &mut footprint.profiles {
            profile.budget.clear();
        }
        footprint.example.budget = crate::config::Budget::default();
        let rows = compare(&footprint, &report.measurements);
        assert!(rows.iter().all(|row| row.verdict == Verdict::MeasureOnly));
        assert!(!any_exceeded(&rows));
    }

    #[test]
    fn report_table_renders() {
        let rows = vec![
            Row {
                subject: String::from("minimal-bridge"),
                target: String::from("x86_64-unknown-linux-gnu"),
                metric: Metric::BinarySize,
                value: Some(3_145_728),
                budget: Some(4_194_304),
                verdict: Verdict::Within,
            },
            Row {
                subject: String::from("onoff_node"),
                target: String::from("x86_64-unknown-linux-gnu"),
                metric: Metric::Rss,
                value: Some(17_825_792),
                budget: Some(16_777_216),
                verdict: Verdict::Exceeded,
            },
            Row {
                subject: String::from("onoff_node"),
                target: String::from("x86_64-unknown-linux-gnu"),
                metric: Metric::RssHighWater,
                value: Some(18_000_000),
                budget: None,
                verdict: Verdict::MeasureOnly,
            },
            Row {
                subject: String::from("full-bridge"),
                target: String::from("armv7-unknown-linux-musleabihf"),
                metric: Metric::BinarySize,
                value: None,
                budget: Some(6_000_000),
                verdict: Verdict::NotMeasured,
            },
        ];
        let notes = vec![(String::from("onoff_node"), String::from("20 devices"))];
        let expected = "\
## Footprint

**Failed**: 1 within budget, 1 exceeded, 1 measured only, 1 not measured.

| Subject | Target | Measurement | Value | Budget | Result |
|---|---|---|---|---|---|
| minimal-bridge | x86_64-unknown-linux-gnu | stripped binary | 3.00 MiB (3_145_728) | 4.00 MiB (4_194_304) | ok (75 %) |
| onoff_node | x86_64-unknown-linux-gnu | RSS (20 devices) | 17.00 MiB (17_825_792) | 16.00 MiB (16_777_216) | **exceeded** (106 %) |
| onoff_node | x86_64-unknown-linux-gnu | peak RSS (VmHWM) (20 devices) | 17.17 MiB (18_000_000) | – | measured only |
| full-bridge | armv7-unknown-linux-musleabihf | stripped binary | – | 5.72 MiB (6_000_000) | not measured |
";
        assert_eq!(render_markdown(&rows, &notes), expected);
        let passed = render_markdown(&rows[..1], &[]);
        assert!(
            passed.contains("Passed: 1 within budget, 0 exceeded"),
            "{passed}"
        );
    }

    #[test]
    fn report_round_trips_through_json() {
        let (_, report) = fixture();
        let text = serde_json::to_string_pretty(&report).expect("serialises");
        let back: Report = serde_json::from_str(&text).expect("parses");
        assert_eq!(back, report);
    }

    #[test]
    fn bytes_are_shown_in_mib_and_exactly() {
        assert_eq!(bytes(0), "0.00 MiB (0)");
        assert_eq!(bytes(1_048_576), "1.00 MiB (1_048_576)");
        assert_eq!(bytes(1_572_864), "1.50 MiB (1_572_864)");
    }
}
