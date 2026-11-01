//! matter-rs: the service binary (layer 7).
//!
//! The binary wires together the engine implementation, the roles and the adapters that its
//! Cargo features select, reads the configuration and runs as a headless service. It is the only
//! package that sees every crate of the workspace. At its top level it may use a boxed dynamic
//! error once there is something that can fail; the library crates each have their own error
//! type.
//!
//! The command line, the service integration and the wiring follow in later steps; today the
//! binary only reports what it was built with.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

/// Name of the project, as used in user-visible output.
const PROJECT_NAME: &str = "matter-rs";

/// Version of the binary, from the package manifest.
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Names of the roles and adapters compiled into this binary, in a fixed order.
fn enabled_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    if cfg!(feature = "role-bridge") {
        features.push("role-bridge");
    }
    if cfg!(feature = "adapter-mqtt") {
        features.push("adapter-mqtt");
    }
    features
}

/// One line that says what this binary is and what it was built with.
fn banner() -> String {
    let features = enabled_features();
    let features = if features.is_empty() {
        String::from("none")
    } else {
        features.join(", ")
    };
    format!("{PROJECT_NAME} {VERSION} (features: {features})")
}

fn main() {
    println!("{}", banner());
}

#[cfg(test)]
mod tests {
    use super::{PROJECT_NAME, banner, enabled_features};

    #[test]
    fn project_name_is_matter_rs() {
        assert_eq!(PROJECT_NAME, "matter-rs");
    }

    #[test]
    fn banner_names_project_and_version() {
        let banner = banner();
        assert!(
            banner.starts_with("matter-rs 0.0.0 (features: "),
            "{banner}"
        );
    }

    #[test]
    fn enabled_features_match_cfg() {
        let features = enabled_features();
        assert_eq!(
            features.contains(&"role-bridge"),
            cfg!(feature = "role-bridge")
        );
        assert_eq!(
            features.contains(&"adapter-mqtt"),
            cfg!(feature = "adapter-mqtt")
        );
    }
}
