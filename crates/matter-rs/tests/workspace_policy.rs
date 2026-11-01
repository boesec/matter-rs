//! Pins the workspace policy that the compiler cannot check from inside a crate: the workspace
//! forbids unsafe code, every crate inherits the workspace lints and passes the unsafe ban on to
//! its doc tests, no crate is published yet, and the `no_std` tier is what the architecture
//! document says it is.

mod common;

use common::{read, workspace_packages, workspace_root};

/// The crates that build without the standard library. They are `no_std` unless their default
/// `std` feature is on; the CI checks them against a bare-metal target.
const NO_STD_TIER: [&str; 3] = [
    "matter-rs-engine",
    "matter-rs-adapter-api",
    "matter-rs-bridge",
];

#[test]
fn workspace_forbids_unsafe_code() {
    let workspace = common::read_toml(&workspace_root().join("Cargo.toml"));
    let unsafe_code = workspace["workspace"]["lints"]["rust"]["unsafe_code"].as_str();
    assert_eq!(
        unsafe_code,
        Some("forbid"),
        "[workspace.lints.rust] must contain unsafe_code = \"forbid\""
    );
}

#[test]
fn workspace_has_every_expected_crate() {
    let names: Vec<String> = workspace_packages()
        .into_iter()
        .map(|package| package.name)
        .collect();
    for expected in [
        "matter-rs-engine",
        "matter-rs-engine-rsmatter",
        "matter-rs-bridge",
        "matter-rs-adapter-api",
        "matter-rs-adapter-mqtt",
        "matter-rs-config",
        "matter-rs",
    ] {
        assert!(
            names.contains(&expected.to_owned()),
            "{expected} is missing from {names:?}"
        );
    }
}

#[test]
fn crate_inherits_workspace_lints() {
    for package in workspace_packages() {
        let lints = package
            .manifest
            .get("lints")
            .and_then(toml::Value::as_table);
        let inherits = lints
            .and_then(|lints| lints.get("workspace"))
            .and_then(toml::Value::as_bool);
        assert_eq!(
            inherits,
            Some(true),
            "{}: [lints] must contain workspace = true",
            package.name
        );
    }
}

#[test]
fn crate_forbids_unsafe_code_in_source_and_doc_tests() {
    for package in workspace_packages() {
        let source = read(&package.root_source);
        for attribute in [
            "#![forbid(unsafe_code)]",
            "#![doc(test(attr(forbid(unsafe_code))))]",
        ] {
            assert!(
                source.lines().any(|line| line.trim() == attribute),
                "{}: {} must contain {attribute}",
                package.name,
                package.root_source.display()
            );
        }
    }
}

#[test]
fn crate_is_not_published() {
    for package in workspace_packages() {
        let publish = package.manifest["package"]
            .get("publish")
            .and_then(toml::Value::as_bool);
        assert_eq!(
            publish,
            Some(false),
            "{}: [package] must contain publish = false",
            package.name
        );
    }
}

#[test]
fn no_std_tier_is_no_std_without_the_default_std_feature() {
    let packages = workspace_packages();
    for name in NO_STD_TIER {
        let package = packages
            .iter()
            .find(|package| package.name == name)
            .unwrap_or_else(|| panic!("{name} is not a workspace package"));

        let features = package.manifest["features"]
            .as_table()
            .unwrap_or_else(|| panic!("{name}: [features] is missing"));
        assert!(
            features.contains_key("std"),
            "{name}: feature `std` is missing"
        );
        let default = features["default"].as_array().expect("default is an array");
        assert!(
            default
                .iter()
                .any(|feature| feature.as_str() == Some("std")),
            "{name}: `std` must be a default feature, found {default:?}"
        );

        let source = read(&package.root_source);
        for line in [
            "#![cfg_attr(not(feature = \"std\"), no_std)]",
            "extern crate alloc;",
        ] {
            assert!(
                source.lines().any(|source_line| source_line.trim() == line),
                "{name}: {} must contain {line}",
                package.root_source.display()
            );
        }
    }
}
