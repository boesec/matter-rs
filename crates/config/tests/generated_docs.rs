//! Keeps the committed JSON schema and configuration reference in step with the config types.
//!
//! The documents describe a build with every role and adapter, so the tests only run when those
//! features are on (as in a workspace build). With the environment variable `MATTER_RS_BLESS`
//! set, the tests rewrite the documents instead of comparing them.

use std::fs;
use std::path::{Path, PathBuf};

const REGENERATE: &str = "set the environment variable MATTER_RS_BLESS=1 and run \
                          `cargo test -p matter-rs-config --all-features --test generated_docs`";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn check(relative: &str, generated: &str) {
    let path = workspace_root().join(relative);
    if std::env::var_os("MATTER_RS_BLESS").is_some() {
        fs::create_dir_all(path.parent().expect("document has a parent directory"))
            .expect("create the document directory");
        fs::write(&path, generated).expect("write the document");
        return;
    }
    let committed = fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "{relative} is stale or missing; {REGENERATE}"
    );
}

#[test]
#[cfg_attr(
    not(all(feature = "role-bridge", feature = "adapter-mqtt")),
    ignore = "the documents describe a build with every role and adapter"
)]
fn schema_is_current() {
    check("doc/config.schema.json", &matter_rs_config::json_schema());
}

#[test]
#[cfg_attr(
    not(all(feature = "role-bridge", feature = "adapter-mqtt")),
    ignore = "the documents describe a build with every role and adapter"
)]
fn reference_is_current() {
    check("doc/config.md", &matter_rs_config::reference_markdown());
}
