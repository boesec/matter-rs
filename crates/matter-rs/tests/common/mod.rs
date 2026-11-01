//! Shared helpers of the workspace tests: locating the workspace, reading its manifests and
//! listing the direct dependencies of every member package.
//!
//! Each integration test is its own crate and uses only part of this module, hence the
//! dead-code allowance.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// One member package of the workspace.
#[derive(Debug, Clone)]
pub struct WorkspacePackage {
    /// The package name, for example `matter-rs-engine`.
    pub name: String,
    /// The package directory relative to the workspace root, with forward slashes, for example
    /// `crates/engine`.
    pub directory: String,
    /// The parsed `Cargo.toml` of the package.
    pub manifest: toml::Table,
    /// The crate root source file (`src/lib.rs` or `src/main.rs`).
    pub root_source: PathBuf,
}

/// The workspace root, derived from this package's manifest directory.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root exists")
}

/// Reads a text file or panics with its path.
pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// Parses a TOML file or panics with its path.
pub fn read_toml(path: &Path) -> toml::Table {
    read(path)
        .parse()
        .unwrap_or_else(|error| panic!("parsing {}: {error}", path.display()))
}

/// Lists the member packages of the workspace, following `workspace.members` of the root
/// manifest. Only the `<dir>/*` form of glob is supported, which is the form the workspace uses.
pub fn workspace_packages() -> Vec<WorkspacePackage> {
    let root = workspace_root();
    let workspace = read_toml(&root.join("Cargo.toml"));
    let members = workspace["workspace"]["members"]
        .as_array()
        .expect("workspace.members is an array");

    let mut directories = Vec::new();
    for member in members {
        let member = member.as_str().expect("member is a string");
        if let Some(parent) = member.strip_suffix("/*") {
            let mut entries: Vec<_> = fs::read_dir(root.join(parent))
                .unwrap_or_else(|error| panic!("listing {parent}: {error}"))
                .map(|entry| entry.expect("directory entry").file_name())
                .filter(|name| root.join(parent).join(name).join("Cargo.toml").is_file())
                .map(|name| format!("{parent}/{}", name.to_string_lossy()))
                .collect();
            entries.sort();
            directories.extend(entries);
        } else {
            directories.push(member.to_owned());
        }
    }

    directories
        .into_iter()
        .map(|directory| {
            let package_dir = root.join(&directory);
            let manifest = read_toml(&package_dir.join("Cargo.toml"));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("package.name is a string")
                .to_owned();
            let root_source = ["src/lib.rs", "src/main.rs"]
                .iter()
                .map(|candidate| package_dir.join(candidate))
                .find(|candidate| candidate.is_file())
                .unwrap_or_else(|| panic!("{name} has neither src/lib.rs nor src/main.rs"));
            WorkspacePackage {
                name,
                directory,
                manifest,
                root_source,
            }
        })
        .collect()
}

/// The package names of every direct dependency declared in `manifest`: normal, dev and build
/// dependencies, including optional ones and target-specific ones. A renamed dependency
/// (`alias = { package = "name" }`) is reported under its package name.
pub fn direct_dependencies(manifest: &toml::Table) -> Vec<String> {
    const KINDS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

    let mut names = Vec::new();
    let mut tables: Vec<&toml::Table> = vec![manifest];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().filter_map(toml::Value::as_table));
    }
    for table in tables {
        for kind in KINDS {
            let Some(dependencies) = table.get(kind).and_then(toml::Value::as_table) else {
                continue;
            };
            for (alias, spec) in dependencies {
                let package = spec
                    .as_table()
                    .and_then(|spec| spec.get("package"))
                    .and_then(toml::Value::as_str)
                    .unwrap_or(alias);
                names.push(package.to_owned());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Returns the lines of the Markdown table rows (lines starting with `|`) in the section of
/// `document` whose `## ` heading contains `heading`, without the header and separator rows.
pub fn markdown_table_rows(document: &str, heading: &str) -> Vec<Vec<String>> {
    let mut in_section = false;
    let mut rows = Vec::new();
    for line in document.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            in_section = title.contains(heading);
            continue;
        }
        if !in_section || !line.starts_with('|') {
            continue;
        }
        let cells: Vec<String> = line
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().to_owned())
            .collect();
        rows.push(cells);
    }
    assert!(
        rows.len() >= 2,
        "no table found in the section whose heading contains {heading:?}"
    );
    // Drop the header row and the separator row.
    rows.into_iter().skip(2).collect()
}

/// Strips the backticks of an inline code cell such as `` `matter-rs-engine` ``.
pub fn unquote(cell: &str) -> &str {
    cell.trim_matches('`')
}
