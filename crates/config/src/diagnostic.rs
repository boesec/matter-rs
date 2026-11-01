//! Errors and warnings about a configuration file, each with its location in the file.

use core::fmt;
use core::ops::Range;

/// How serious a [`Diagnostic`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// The file is invalid; the service does not start with it.
    Error,
    /// The file is valid, but something in it is probably not what was meant.
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Error => "error",
            Self::Warning => "warning",
        })
    }
}

/// A position in a file: line and column, both counted from 1. The column counts characters, not
/// bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    /// Line number, starting at 1.
    pub line: usize,
    /// Column number in characters, starting at 1.
    pub column: usize,
}

impl Location {
    /// The location of the byte `offset` in `text`. An offset past the end, or inside a
    /// multi-byte character, is moved back to the nearest character start.
    #[must_use]
    pub fn from_offset(text: &str, offset: usize) -> Self {
        let mut offset = offset.min(text.len());
        while !text.is_char_boundary(offset) {
            offset -= 1;
        }
        let before = &text[..offset];
        let line = before.matches('\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let column = before[line_start..].chars().count() + 1;
        Self { line, column }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// One error or warning about a configuration file.
///
/// It displays as `<file>:<line>:<column>: <severity>: <message>`. Messages never contain the
/// value of a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Error or warning.
    pub severity: Severity,
    /// The file name as it was given to the loader.
    pub file: String,
    /// Where in the file the problem is.
    pub location: Location,
    /// What the problem is.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.file, self.location, self.severity, self.message
        )
    }
}

/// Every error and warning found in one configuration file, ordered by location.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    /// All diagnostics, errors and warnings, ordered by location.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The errors only.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
    }

    /// The warnings only.
    pub fn warnings(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Warning)
    }

    /// Whether the report contains at least one error.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for diagnostic in &self.diagnostics {
            writeln!(f, "{diagnostic}")?;
        }
        Ok(())
    }
}

/// Collects diagnostics while a file is checked and turns byte spans into locations.
pub(crate) struct Collector<'a> {
    file: &'a str,
    text: &'a str,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Collector<'a> {
    pub(crate) fn new(file: &'a str, text: &'a str) -> Self {
        Self {
            file,
            text,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn error(&mut self, span: &Range<usize>, message: impl Into<String>) {
        self.push(Severity::Error, span, message.into());
    }

    pub(crate) fn warning(&mut self, span: &Range<usize>, message: impl Into<String>) {
        self.push(Severity::Warning, span, message.into());
    }

    fn push(&mut self, severity: Severity, span: &Range<usize>, message: String) {
        self.diagnostics.push(Diagnostic {
            severity,
            file: self.file.to_owned(),
            location: Location::from_offset(self.text, span.start),
            message,
        });
    }

    pub(crate) fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count()
    }

    /// Takes the diagnostics collected since `mark` (a value of [`Collector::len`]).
    pub(crate) fn split_off(&mut self, mark: usize) -> Vec<Diagnostic> {
        self.diagnostics.split_off(mark)
    }

    pub(crate) fn len(&self) -> usize {
        self.diagnostics.len()
    }

    pub(crate) fn extend(&mut self, diagnostics: Vec<Diagnostic>) {
        self.diagnostics.extend(diagnostics);
    }

    pub(crate) fn into_report(self) -> Report {
        let mut diagnostics = self.diagnostics;
        diagnostics.sort_by(|a, b| {
            (a.location, a.severity, &a.message).cmp(&(b.location, b.severity, &b.message))
        });
        diagnostics.dedup();
        Report { diagnostics }
    }
}

#[cfg(test)]
mod tests {
    use super::{Collector, Location, Severity};

    #[test]
    fn location_counts_lines_and_characters_from_one() {
        let text = "a = 1\nname = \"äöü\" x\n";
        assert_eq!(
            Location::from_offset(text, 0),
            Location { line: 1, column: 1 }
        );
        assert_eq!(
            Location::from_offset(text, 6),
            Location { line: 2, column: 1 }
        );
        // The `x` after three two-byte characters: byte 21, character column 14.
        let x = text.find(" x").unwrap() + 1;
        assert_eq!(
            Location::from_offset(text, x),
            Location {
                line: 2,
                column: 14
            }
        );
    }

    #[test]
    fn location_clamps_offsets_past_the_end_and_inside_characters() {
        let text = "ä";
        assert_eq!(
            Location::from_offset(text, 99),
            Location { line: 1, column: 2 }
        );
        assert_eq!(
            Location::from_offset(text, 1),
            Location { line: 1, column: 1 }
        );
    }

    #[test]
    fn report_is_ordered_by_location_and_displays_file_line_column() {
        let text = "a = 1\nb = 2\n";
        let mut collector = Collector::new("config.toml", text);
        collector.warning(&(6..7), "second");
        collector.error(&(0..1), "first");
        let report = collector.into_report();
        let lines: Vec<String> = report
            .diagnostics()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            lines,
            [
                "config.toml:1:1: error: first",
                "config.toml:2:1: warning: second"
            ]
        );
        assert!(report.has_errors());
        assert_eq!(report.warnings().count(), 1);
        assert_eq!(report.diagnostics()[0].severity, Severity::Error);
    }
}
