//! Pins the workspace policy that the compiler cannot check from inside a crate: the workspace
//! forbids unsafe code and this crate inherits the workspace lints.

use std::fs;
use std::path::Path;

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// Returns the lines of the TOML table `header`, without the header itself.
fn table<'a>(manifest: &'a str, header: &str) -> Vec<&'a str> {
    manifest
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .collect()
}

#[test]
fn workspace_forbids_unsafe_code() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = read(&manifest_dir.join("../../Cargo.toml"));
    let lints = table(&workspace, "[workspace.lints.rust]");
    assert!(
        lints.contains(&r#"unsafe_code = "forbid""#),
        "[workspace.lints.rust] must contain unsafe_code = \"forbid\", found {lints:?}"
    );
}

#[test]
fn crate_inherits_workspace_lints() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = read(&manifest_dir.join("Cargo.toml"));
    let lints = table(&manifest, "[lints]");
    assert!(
        lints.contains(&"workspace = true"),
        "[lints] must contain workspace = true, found {lints:?}"
    );
}
