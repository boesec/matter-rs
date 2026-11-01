//! Keeps the public architecture document and the workspace in step: every workspace package
//! appears in the crate map of `ARCHITECTURE.md` with its real directory, every crate the
//! document calls planned does not exist yet, and the dependency-rule table of the document names
//! the same crates as the crate map.

mod common;

use std::collections::BTreeSet;

use common::{markdown_table_rows, read, unquote, workspace_packages, workspace_root};

/// One row of the crate map.
struct CrateRow {
    package: String,
    directory: String,
    status: String,
}

fn architecture_document() -> String {
    read(&workspace_root().join("ARCHITECTURE.md"))
}

/// The rows of the crate map table in the "Crate map" section.
fn crate_map(document: &str) -> Vec<CrateRow> {
    markdown_table_rows(document, "Crate map")
        .into_iter()
        .map(|cells| {
            assert!(
                cells.len() >= 6,
                "crate map row has too few columns: {cells:?}"
            );
            CrateRow {
                package: unquote(&cells[0]).to_owned(),
                directory: unquote(&cells[1]).to_owned(),
                status: cells[5].clone(),
            }
        })
        .collect()
}

#[test]
fn architecture_lists_every_crate() {
    let document = architecture_document();
    let rows = crate_map(&document);
    for package in workspace_packages() {
        let row = rows
            .iter()
            .find(|row| row.package == package.name)
            .unwrap_or_else(|| panic!("{} is missing from the crate map", package.name));
        assert_eq!(
            row.directory, package.directory,
            "{}: the crate map names a different directory",
            package.name
        );
        assert!(
            !row.status.starts_with("planned"),
            "{}: exists but the crate map calls it planned",
            package.name
        );
    }
}

#[test]
fn architecture_planned_crates_do_not_exist_yet() {
    let document = architecture_document();
    let existing: BTreeSet<String> = workspace_packages()
        .into_iter()
        .map(|package| package.name)
        .collect();
    let planned: Vec<CrateRow> = crate_map(&document)
        .into_iter()
        .filter(|row| row.status.starts_with("planned"))
        .collect();
    assert!(!planned.is_empty(), "the crate map lists no planned crate");
    for row in planned {
        assert!(
            !existing.contains(&row.package),
            "{}: the crate map calls it planned but it is a workspace package",
            row.package
        );
        assert!(
            !workspace_root().join(&row.directory).exists(),
            "{}: the crate map calls it planned but {} exists",
            row.package,
            row.directory
        );
    }
}

#[test]
fn architecture_dependency_table_matches_crate_map() {
    let document = architecture_document();
    let map: BTreeSet<String> = crate_map(&document)
        .into_iter()
        .map(|row| row.package)
        .collect();
    let rules: BTreeSet<String> = markdown_table_rows(&document, "Dependency rules")
        .into_iter()
        .map(|cells| unquote(&cells[0]).to_owned())
        .collect();
    assert_eq!(
        rules, map,
        "the dependency-rule table and the crate map name different crates"
    );
}
