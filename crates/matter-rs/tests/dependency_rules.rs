//! Enforces the dependency rules between the workspace crates: lower layers never depend on
//! higher ones, and only the rs-matter engine implementation depends on rs-matter. The rule
//! table below is the one source of truth in code; the same table is printed in
//! `ARCHITECTURE.md`, and a sibling test keeps the two in step.
//!
//! The checker is pure (packages in, violations out) so that it can be tested with fabricated
//! violations: a test that only ever sees the real, correct workspace would not prove that it
//! fails when it should.

mod common;

use std::fmt;

use common::{direct_dependencies, workspace_packages};

/// The only crate that may depend on rs-matter.
const ENGINE_RSMATTER: &str = "matter-rs-engine-rsmatter";

/// Prefix of every workspace crate. A dependency with this prefix is treated as a workspace crate
/// even when it does not exist yet, so a crate that is planned but not created is still ruled.
const WORKSPACE_PREFIX: &str = "matter-rs";

/// Direct dependencies allowed between workspace crates: for every crate, the workspace crates it
/// may name in `[dependencies]`, `[dev-dependencies]` or `[build-dependencies]`. Crates that do
/// not exist yet are listed so that the rule is in place when they come. The engine
/// implementation is reserved for the binary: only the service wires an engine in.
const RULES: &[(&str, &[&str])] = &[
    ("matter-rs-engine", &[]),
    (ENGINE_RSMATTER, &["matter-rs-engine"]),
    ("matter-rs-bridge", &["matter-rs-engine"]),
    ("matter-rs-controller", &["matter-rs-engine"]),
    (
        "matter-rs-adapter-api",
        &[
            "matter-rs-engine",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
    (
        "matter-rs-adapter-mqtt",
        &[
            "matter-rs-adapter-api",
            "matter-rs-engine",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
    (
        "matter-rs-config",
        &[
            "matter-rs-adapter-api",
            "matter-rs-adapter-mqtt",
            "matter-rs-engine",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
    (
        "matter-rs-mgmt-api",
        &[
            "matter-rs-config",
            "matter-rs-adapter-api",
            "matter-rs-adapter-mqtt",
            "matter-rs-engine",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
    (
        "matter-rs-ui",
        &[
            "matter-rs-mgmt-api",
            "matter-rs-config",
            "matter-rs-adapter-api",
            "matter-rs-adapter-mqtt",
            "matter-rs-engine",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
    (
        "matter-rs",
        &[
            "matter-rs-ui",
            "matter-rs-mgmt-api",
            "matter-rs-config",
            "matter-rs-adapter-api",
            "matter-rs-adapter-mqtt",
            "matter-rs-engine",
            "matter-rs-engine-rsmatter",
            "matter-rs-bridge",
            "matter-rs-controller",
        ],
    ),
];

/// A package as the checker sees it: its name and the names of its direct dependencies.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Package {
    name: String,
    dependencies: Vec<String>,
}

impl Package {
    fn new(name: &str, dependencies: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            dependencies: dependencies.iter().map(|&dep| dep.to_owned()).collect(),
        }
    }
}

/// A broken rule.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Violation {
    /// A workspace crate has no row in the rule table.
    UnruledCrate { package: String },
    /// A crate depends on a workspace crate outside its allowed set.
    ForbiddenWorkspaceDependency { package: String, dependency: String },
    /// A crate other than the engine implementation depends on rs-matter.
    RsMatterOutsideEngine { package: String, dependency: String },
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnruledCrate { package } => {
                write!(f, "{package} has no row in the dependency rule table")
            }
            Self::ForbiddenWorkspaceDependency {
                package,
                dependency,
            } => write!(f, "{package} must not depend on {dependency}"),
            Self::RsMatterOutsideEngine {
                package,
                dependency,
            } => write!(
                f,
                "{package} depends on {dependency}; only {ENGINE_RSMATTER} may depend on rs-matter"
            ),
        }
    }
}

fn is_workspace_crate(name: &str) -> bool {
    name == WORKSPACE_PREFIX || name.starts_with(&format!("{WORKSPACE_PREFIX}-"))
}

fn is_rs_matter(name: &str) -> bool {
    name == "rs-matter" || name.starts_with("rs-matter-")
}

/// Checks the layering: every package is ruled, and every workspace dependency is allowed.
fn check_layering(packages: &[Package]) -> Vec<Violation> {
    let mut violations = Vec::new();
    for package in packages {
        let Some((_, allowed)) = RULES.iter().find(|(name, _)| *name == package.name) else {
            violations.push(Violation::UnruledCrate {
                package: package.name.clone(),
            });
            continue;
        };
        for dependency in &package.dependencies {
            if is_workspace_crate(dependency) && !allowed.contains(&dependency.as_str()) {
                violations.push(Violation::ForbiddenWorkspaceDependency {
                    package: package.name.clone(),
                    dependency: dependency.clone(),
                });
            }
        }
    }
    violations
}

/// Checks the isolation rule: only the engine implementation names rs-matter or an `rs-matter-*`
/// crate as a direct dependency.
fn check_rs_matter_isolation(packages: &[Package]) -> Vec<Violation> {
    packages
        .iter()
        .filter(|package| package.name != ENGINE_RSMATTER)
        .flat_map(|package| {
            package
                .dependencies
                .iter()
                .filter(|dependency| is_rs_matter(dependency))
                .map(|dependency| Violation::RsMatterOutsideEngine {
                    package: package.name.clone(),
                    dependency: dependency.clone(),
                })
        })
        .collect()
}

fn real_packages() -> Vec<Package> {
    workspace_packages()
        .iter()
        .map(|package| Package {
            name: package.name.clone(),
            dependencies: direct_dependencies(&package.manifest),
        })
        .collect()
}

fn assert_no_violations(violations: &[Violation]) {
    let report: Vec<String> = violations.iter().map(ToString::to_string).collect();
    assert!(
        violations.is_empty(),
        "dependency rules broken:\n{}",
        report.join("\n")
    );
}

#[test]
fn workspace_respects_layering() {
    let packages = real_packages();
    assert_no_violations(&check_layering(&packages));
    // Guard against a vacuous pass: the real workspace does have dependencies between its crates.
    let workspace_edges = packages
        .iter()
        .flat_map(|package| &package.dependencies)
        .filter(|dependency| is_workspace_crate(dependency))
        .count();
    assert!(
        workspace_edges > 0,
        "no workspace dependencies found; is the manifest reader broken?"
    );
}

#[test]
fn only_engine_rsmatter_may_depend_on_rs_matter() {
    assert_no_violations(&check_rs_matter_isolation(&real_packages()));
}

#[test]
fn rule_table_names_every_workspace_crate_exactly_once() {
    let mut ruled: Vec<&str> = RULES.iter().map(|(name, _)| *name).collect();
    ruled.sort_unstable();
    ruled.dedup();
    assert_eq!(ruled.len(), RULES.len(), "a crate appears twice in RULES");
    for (name, allowed) in RULES {
        assert!(
            is_workspace_crate(name),
            "{name} is not a workspace crate name"
        );
        for dependency in *allowed {
            assert!(
                RULES.iter().any(|(ruled, _)| ruled == dependency),
                "{name} may depend on {dependency}, which has no row of its own"
            );
            assert_ne!(name, dependency, "{name} may not depend on itself");
        }
    }
}

#[test]
fn checker_accepts_a_correct_workspace() {
    let packages = vec![
        Package::new("matter-rs-engine", &["thiserror"]),
        Package::new(
            ENGINE_RSMATTER,
            &["matter-rs-engine", "rs-matter", "rs-matter-macros"],
        ),
        Package::new("matter-rs-bridge", &["matter-rs-engine"]),
        Package::new(
            "matter-rs",
            &["matter-rs-engine", ENGINE_RSMATTER, "matter-rs-bridge"],
        ),
    ];
    assert_eq!(check_layering(&packages), vec![]);
    assert_eq!(check_rs_matter_isolation(&packages), vec![]);
}

#[test]
fn checker_rejects_upward_dependency() {
    let packages = vec![
        Package::new("matter-rs-engine", &["matter-rs-bridge"]),
        Package::new("matter-rs-bridge", &["matter-rs-engine"]),
    ];
    assert_eq!(
        check_layering(&packages),
        vec![Violation::ForbiddenWorkspaceDependency {
            package: "matter-rs-engine".to_owned(),
            dependency: "matter-rs-bridge".to_owned(),
        }]
    );
}

#[test]
fn checker_rejects_dependency_on_engine_implementation_outside_binary() {
    let packages = vec![Package::new(
        "matter-rs-bridge",
        &["matter-rs-engine", ENGINE_RSMATTER],
    )];
    assert_eq!(
        check_layering(&packages),
        vec![Violation::ForbiddenWorkspaceDependency {
            package: "matter-rs-bridge".to_owned(),
            dependency: ENGINE_RSMATTER.to_owned(),
        }]
    );
}

#[test]
fn checker_rejects_unruled_crate() {
    let packages = vec![Package::new("matter-rs-common", &[])];
    assert_eq!(
        check_layering(&packages),
        vec![Violation::UnruledCrate {
            package: "matter-rs-common".to_owned(),
        }]
    );
}

#[test]
fn checker_rejects_rs_matter_outside_engine_rsmatter() {
    let packages = vec![
        Package::new("matter-rs-bridge", &["matter-rs-engine", "rs-matter"]),
        Package::new("matter-rs", &["rs-matter-macros"]),
        Package::new(ENGINE_RSMATTER, &["rs-matter"]),
    ];
    assert_eq!(
        check_rs_matter_isolation(&packages),
        vec![
            Violation::RsMatterOutsideEngine {
                package: "matter-rs-bridge".to_owned(),
                dependency: "rs-matter".to_owned(),
            },
            Violation::RsMatterOutsideEngine {
                package: "matter-rs".to_owned(),
                dependency: "rs-matter-macros".to_owned(),
            },
        ]
    );
}
