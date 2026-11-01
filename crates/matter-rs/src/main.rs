//! matter-rs: the service binary (layer 7).
//!
//! The binary wires together the engine implementation, the roles and the adapters that its
//! Cargo features select, reads the configuration and runs as a headless service. It is the only
//! package that sees every crate of the workspace. At its top level it may use a boxed dynamic
//! error; the library crates each have their own error type.
//!
//! Today the command line covers the configuration: writing a starter file, validating a file,
//! printing its schema and reference, printing the pairing information of the configured node,
//! and listing the compiled-in features. Running the service follows in a later step.
//!
//! Exit codes: 0 on success (a valid file, warnings allowed), 1 when the command failed (an
//! invalid or unreadable file, a file that already exists), 2 on wrong usage.

#![forbid(unsafe_code)]
#![doc(test(attr(forbid(unsafe_code))))]

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use matter_rs_config::{
    CONFIG_ENV, Error, Loaded, SystemEnvironment, json_schema, load, new_starter_config,
    reference_markdown, resolve_config_path,
};
use matter_rs_engine::{NodeIdentity, ProductId, VendorId};
use matter_rs_engine_rsmatter::pairing_info_for;

/// Exit code of a command that failed.
const EXIT_FAILURE: u8 = 1;

/// Matter bridge and controller.
#[derive(Debug, Parser)]
#[command(name = "matter-rs", version, about, arg_required_else_help = true)]
struct Cli {
    // The help text is given as an attribute because it names a variable and paths verbatim.
    #[arg(
        long,
        global = true,
        value_name = "PATH",
        help = "Configuration file. Without it, the environment variable MATTER_RS_CONFIG is \
                used, then the platform default (/etc/matter-rs/config.toml, \
                %ProgramData%\\matter-rs\\config.toml)"
    )]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Write a commented starter configuration with a fresh passcode and discriminator.
    Init {
        /// Where to write it. Without it, the configuration file path is used.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Check a configuration file and report every error and warning with its location.
    Validate {
        /// The file to check. Without it, the configuration file path is used.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// Print the pairing information of the node in a configuration file: the manual pairing
    /// code, the QR code text and the discriminator. The codes encode the setup passcode and
    /// are shown here and nowhere else.
    PairingInfo {
        /// The file to read. Without it, the configuration file path is used.
        #[arg(value_name = "PATH")]
        path: Option<PathBuf>,
    },
    /// List the features compiled into this binary, one per line.
    Features,
    /// Print the JSON schema of the configuration file.
    Schema {
        /// Print the configuration reference as Markdown instead.
        #[arg(long)]
        markdown: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Init { output, force } => init(output.or(cli.config), force),
        Command::Validate { path } => {
            if path.is_some() && cli.config.is_some() {
                Cli::command()
                    .error(
                        ErrorKind::ArgumentConflict,
                        "give the file either as PATH or with --config, not both",
                    )
                    .exit();
            }
            validate(path.or(cli.config))
        }
        Command::PairingInfo { path } => {
            if path.is_some() && cli.config.is_some() {
                Cli::command()
                    .error(
                        ErrorKind::ArgumentConflict,
                        "give the file either as PATH or with --config, not both",
                    )
                    .exit();
            }
            pairing_info(path.or(cli.config))
        }
        Command::Features => {
            for feature in enabled_features() {
                println!("{feature}");
            }
            ExitCode::SUCCESS
        }
        Command::Schema { markdown } => {
            if markdown {
                print!("{}", reference_markdown());
            } else {
                print!("{}", json_schema());
            }
            ExitCode::SUCCESS
        }
    }
}

/// Names of the features compiled into this binary, in a fixed order.
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

/// The configuration file to use, or a message why there is none.
fn config_path(given: Option<PathBuf>) -> Result<PathBuf, String> {
    resolve_config_path(given, &SystemEnvironment)
        .map(|(path, _)| path)
        .ok_or_else(|| {
            format!(
                "no configuration file given and no platform default available; use --config or \
                 set {CONFIG_ENV}"
            )
        })
}

fn init(output: Option<PathBuf>, force: bool) -> ExitCode {
    let path = match config_path(output) {
        Ok(path) => path,
        Err(message) => return fail(&message),
    };
    let text = match new_starter_config() {
        Ok(text) => text,
        Err(error) => return fail(&error.to_string()),
    };
    match write_private(&path, &text, force) {
        Ok(()) => {
            println!("wrote a starter configuration to {}", path.display());
            println!("check it with: matter-rs validate {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => fail(&format!(
            "{} already exists; use --force to overwrite it",
            path.display()
        )),
        Err(error) => fail(&format!("cannot write {}: {error}", path.display())),
    }
}

/// Writes a file that only its owner may read on Unix, because it holds a secret. Creates the
/// parent directory. Without `force` an existing file is an error.
fn write_private(path: &Path, text: &str, force: bool) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true);
    if force {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

fn validate(given: Option<PathBuf>) -> ExitCode {
    let path = match config_path(given) {
        Ok(path) => path,
        Err(message) => return fail(&message),
    };
    match load(&path, &SystemEnvironment) {
        Ok(loaded) => {
            print_valid(&path, &loaded);
            ExitCode::SUCCESS
        }
        Err(Error::Invalid(report)) => {
            eprint!("{report}");
            let errors = report.errors().count();
            let warnings = report.warnings().count();
            println!(
                "{}: invalid, {errors} {}, {warnings} {}",
                path.display(),
                plural(errors, "error", "errors"),
                plural(warnings, "warning", "warnings")
            );
            ExitCode::from(EXIT_FAILURE)
        }
        Err(error) => fail(&error_chain(&error)),
    }
}

/// Prints the pairing information of the configured node. This is the one place the pairing
/// code and the QR text are shown; `validate` and the logs never print them.
fn pairing_info(given: Option<PathBuf>) -> ExitCode {
    let path = match config_path(given) {
        Ok(path) => path,
        Err(message) => return fail(&message),
    };
    let loaded = match load(&path, &SystemEnvironment) {
        Ok(loaded) => loaded,
        Err(Error::Invalid(report)) => {
            eprint!("{report}");
            return fail(&format!("{} is not valid", path.display()));
        }
        Err(error) => return fail(&error_chain(&error)),
    };
    let node = &loaded.config.node;
    // Only the vendor and product ids, the serial number, the passcode and the discriminator
    // enter the codes; the remaining fields of the identity do not and are filled from the
    // binary.
    let identity = NodeIdentity {
        vendor_id: VendorId::new(node.vendor_id),
        product_id: ProductId::new(node.product_id),
        vendor_name: node.vendor_name.clone(),
        product_name: node.product_name.clone(),
        node_label: node.node_label.clone(),
        serial_number: node.serial_number.clone(),
        software_version: 0,
        software_version_string: String::from(env!("CARGO_PKG_VERSION")),
        unique_id: String::new(),
        passcode: node.passcode.clone(),
        discriminator: node.discriminator,
    };
    match pairing_info_for(&identity) {
        Ok(info) => {
            println!("configuration: {}", path.display());
            println!("discriminator: {}", info.discriminator);
            println!("manual pairing code: {}", info.manual_code.expose());
            println!("qr code text: {}", info.qr_text.expose());
            ExitCode::SUCCESS
        }
        Err(error) => fail(&error_chain(&error)),
    }
}

fn print_valid(path: &Path, loaded: &Loaded) {
    eprint!("{}", loaded.report);
    let warnings = loaded.report.warnings().count();
    println!(
        "{}: valid, {warnings} {}",
        path.display(),
        plural(warnings, "warning", "warnings")
    );
    let state = &loaded.config.state;
    println!(
        "state directory: {} ({}, not created by validate)",
        state.dir.display(),
        if state.is_default {
            "default"
        } else {
            "configured"
        }
    );
    let sections: Vec<String> = loaded
        .sections
        .iter()
        .map(|section| match (section.as_str(), &loaded.config.bridge) {
            ("bridge", Some(bridge)) if !bridge.enabled => String::from("bridge (disabled)"),
            _ => section.clone(),
        })
        .collect();
    println!("sections: {}", sections.join(", "));
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

/// An error with its sources, separated by colons.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn fail(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(EXIT_FAILURE)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::{Cli, enabled_features, error_chain, plural};

    #[test]
    fn command_line_definition_is_consistent() {
        Cli::command().debug_assert();
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

    #[test]
    fn error_chain_joins_sources() {
        let error = matter_rs_config::Error::Read {
            path: "x.toml".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "gone"),
        };
        assert_eq!(
            error_chain(&error),
            "cannot read the configuration file `x.toml`: gone"
        );
    }

    #[test]
    fn plural_picks_the_form() {
        assert_eq!(plural(1, "error", "errors"), "error");
        assert_eq!(plural(0, "error", "errors"), "errors");
    }
}
