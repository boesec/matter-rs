//! Loading tests. They run inside the crate so that they can use the test adapter kinds, a fake
//! environment and a build description with features switched off.

use std::path::{Path, PathBuf};

use matter_rs_engine::Secret;

use super::{Build, ConfigSource, Loaded, load_with_build};
use crate::adapters::{AdapterConfig, TestAdapterConfig, adapter_kinds};
use crate::diagnostic::{Diagnostic, Location, Severity};
use crate::environment::fake::FakeEnvironment;
use crate::error::Error;

const FILE: &str = "config.toml";

/// A valid node section; tests append to it or replace lines in it.
const MINIMAL: &str = "[node]
passcode = 13572468
discriminator = 3840
";

fn environment() -> FakeEnvironment {
    FakeEnvironment::default().with_var("HOME", "/home/someone")
}

fn build() -> Build {
    Build {
        role_bridge: true,
        adapter_kinds: adapter_kinds(),
        linux: true,
    }
}

fn source(text: &str) -> ConfigSource<'_> {
    ConfigSource {
        name: FILE,
        text,
        directory: None,
        mode: None,
    }
}

fn load_text(text: &str) -> Result<Loaded, Error> {
    load_with_build(&source(text), &environment(), &build())
}

fn loaded(text: &str) -> Loaded {
    match load_text(text) {
        Ok(loaded) => loaded,
        Err(Error::Invalid(report)) => panic!("unexpected errors:\n{report}"),
        Err(error) => panic!("unexpected error: {error}"),
    }
}

/// The errors of an invalid file.
fn errors(text: &str) -> Vec<Diagnostic> {
    errors_with(text, &environment(), &build())
}

fn errors_with(text: &str, environment: &FakeEnvironment, build: &Build) -> Vec<Diagnostic> {
    match load_with_build(&source(text), environment, build) {
        Ok(loaded) => panic!("expected errors, the file loaded: {loaded:?}"),
        Err(Error::Invalid(report)) => {
            assert!(report.has_errors());
            report.errors().cloned().collect()
        }
        Err(error) => panic!("unexpected error: {error}"),
    }
}

fn warnings(loaded: &Loaded) -> Vec<String> {
    loaded.report.warnings().map(ToString::to_string).collect()
}

/// Asserts that exactly one error exists and returns it.
fn single_error(text: &str) -> Diagnostic {
    let errors = errors(text);
    assert_eq!(errors.len(), 1, "expected one error, got {errors:#?}");
    errors.into_iter().next().unwrap()
}

fn at(line: usize, column: usize) -> Location {
    Location { line, column }
}

#[test]
fn minimal_file_loads_with_defaults() {
    let loaded = loaded(MINIMAL);
    let config = &loaded.config;
    assert_eq!(config.node.vendor_id, 0xFFF1);
    assert_eq!(config.node.product_id, 0x8001);
    assert_eq!(config.node.discriminator, 3840);
    assert_eq!(*config.node.passcode.expose(), 13_572_468);
    assert_eq!(config.network.port, 5540);
    assert!(config.network.ipv4 && config.network.ipv6);
    assert_eq!(config.log.level, crate::LogLevel::Info);
    assert!(config.bridge.is_none());
    assert!(config.adapters.is_empty());
    assert_eq!(
        config.state.dir,
        PathBuf::from("/home/someone/.local/state/matter-rs")
    );
    assert!(config.state.is_default);
    assert_eq!(loaded.sections, ["node"]);
    assert!(loaded.report.diagnostics().is_empty());
}

#[test]
fn full_file_loads() {
    let text = format!(
        "{MINIMAL}vendor_id = 0xFFF2
product_id = 0x8_002
serial_number = \"0001\"

[network]
interface = \"eth0\"
ipv4 = false
port = 5541

[state]
dir = \"/srv/matter\"

[log]
level = \"debug\"
format = \"json\"

[bridge]
enabled = false

[adapters.home]
kind = \"test\"
topic = \"home\"
level = 3
"
    );
    let loaded = loaded(&text);
    let config = &loaded.config;
    assert_eq!(config.node.vendor_id, 0xFFF2);
    assert_eq!(config.node.product_id, 0x8002);
    assert_eq!(config.node.serial_number.as_deref(), Some("0001"));
    assert_eq!(config.network.interface.as_deref(), Some("eth0"));
    assert!(!config.network.ipv4);
    assert_eq!(config.state.dir, PathBuf::from("/srv/matter"));
    assert!(!config.state.is_default);
    assert_eq!(config.log.format, crate::LogFormat::Json);
    assert_eq!(config.bridge.as_ref().map(|b| b.enabled), Some(false));
    assert_eq!(
        config.adapters["home"],
        AdapterConfig::Test(TestAdapterConfig {
            topic: String::from("home"),
            level: 3
        })
    );
    assert_eq!(config.adapters["home"].kind(), "test");
    assert_eq!(
        loaded.sections,
        ["adapters.home", "bridge", "log", "network", "node", "state"]
    );
}

#[test]
fn syntax_error_has_location() {
    let text = "[node]\npasscode = 13572468\ndiscriminator = \n";
    let error = single_error(text);
    assert_eq!(error.severity, Severity::Error);
    assert_eq!(error.file, FILE);
    assert_eq!(error.location.line, 3);
    assert!(error.to_string().starts_with("config.toml:3:"), "{error}");
}

#[test]
fn every_syntax_error_is_reported() {
    let text = "[node]\npasscode = \ndiscriminator = unquoted\n";
    let errors = errors(text);
    assert!(errors.len() >= 2, "{errors:#?}");
    assert!(errors.iter().any(|e| e.location.line == 2), "{errors:#?}");
    assert!(errors.iter().any(|e| e.location.line == 3), "{errors:#?}");
}

#[test]
fn range_error_has_location() {
    let text = "[node]\npasscode = 13572468\ndiscriminator = 5000\n";
    let error = single_error(text);
    assert_eq!(error.location, at(3, 17));
    assert_eq!(
        error.to_string(),
        "config.toml:3:17: error: `node.discriminator` must be between 0 and 4095"
    );
}

#[test]
fn all_errors_reported() {
    let text = "[node]
passcode = 11111111
discriminator = 5000
vendor_name = \"a name that is much longer than thirty-two characters\"
colour = \"blue\"

[network]
port = \"5540\"

[log]
level = \"verbose\"

[unknown]
";
    let errors = errors(text);
    let found: Vec<(usize, &str)> = errors
        .iter()
        .map(|error| (error.location.line, error.message.as_str()))
        .collect();
    let expected_lines = [2, 3, 4, 5, 8, 11, 13];
    assert_eq!(
        found.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
        expected_lines,
        "{found:#?}"
    );
    assert!(found[0].1.contains("forbids"));
    assert!(found[1].1.contains("node.discriminator"));
    assert!(found[2].1.contains("at most 32 characters"));
    assert!(found[3].1.contains("`colour`"));
    assert!(found[4].1.contains("must be an integer, found a string"));
    assert!(found[5].1.contains("`\"info\"`"));
    assert!(found[6].1.contains("`unknown`"));
}

#[test]
fn missing_required_keys_are_reported_at_their_table() {
    let errors = errors("[node]\nvendor_name = \"x\"\n");
    let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "config.toml:1:1: error: missing required key `discriminator` in `[node]`",
            "config.toml:1:1: error: missing required key `passcode` in `[node]`",
        ]
    );
    let error = single_error("[log]\n");
    assert_eq!(
        error.to_string(),
        "config.toml:1:1: error: missing required table `[node]`"
    );
}

#[test]
fn unknown_key_rejected() {
    let error = single_error(&format!("{MINIMAL}colour = \"blue\"\n"));
    assert_eq!(error.location, at(4, 1));
    assert!(
        error
            .message
            .starts_with("unknown key `colour` in `[node]`; expected one of `vendor_id`"),
        "{error}"
    );

    let error = single_error(&format!("verbose = true\n{MINIMAL}"));
    assert_eq!(error.location, at(1, 1));
    assert!(
        error.message.starts_with("unknown key `verbose`;"),
        "{error}"
    );
}

#[test]
fn unknown_kind_rejected() {
    let error = single_error(&format!("{MINIMAL}[adapters.home]\nkind = \"zigbee\"\n"));
    assert_eq!(error.location, at(5, 8));
    assert_eq!(
        error.message,
        "unknown adapter kind `zigbee` in `[adapters.home]`; known kinds are `mqtt`, `test`, `absent`"
    );
}

#[test]
fn kind_not_compiled_in_names_feature() {
    let error = single_error(&format!("{MINIMAL}[adapters.home]\nkind = \"absent\"\n"));
    assert_eq!(error.location, at(5, 8));
    assert!(error.message.contains("`absent`"), "{error}");
    assert!(error.message.contains("`adapter-absent`"), "{error}");
}

#[test]
fn adapter_keys_are_checked_against_their_kind() {
    let text = format!(
        "{MINIMAL}[adapters.home]\nkind = \"test\"\nlevel = 11\nqos = 1\n\n[adapters.away]\ntopic = \"x\"\n"
    );
    let messages: Vec<String> = errors(&text).iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "config.toml:4:1: error: missing required key `topic` in `[adapters.home]`",
            "config.toml:6:9: error: `adapters.home.level` must be between 0 and 10",
            "config.toml:7:1: error: unknown key `qos` in `[adapters.home]`; expected one of \
             `kind`, `topic`, `level`",
            "config.toml:9:1: error: missing required key `kind` in `[adapters.away]`; it \
             selects the adapter",
        ]
    );
}

#[test]
fn adapter_id_must_be_a_simple_name() {
    let error = single_error(&format!(
        "{MINIMAL}[adapters.\"Home Hub\"]\nkind = \"test\"\ntopic = \"x\"\n"
    ));
    assert!(error.message.contains("adapter id `Home Hub`"), "{error}");
}

#[test]
fn bridge_without_role_names_feature() {
    let build = Build {
        role_bridge: false,
        ..build()
    };
    let errors = errors_with(&format!("{MINIMAL}[bridge]\n"), &environment(), &build);
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].location, at(4, 2));
    assert!(errors[0].message.contains("`role-bridge`"), "{}", errors[0]);
}

#[test]
fn devices_without_role_names_feature() {
    let build = Build {
        role_bridge: false,
        ..build()
    };
    let errors = errors_with(
        &format!("{MINIMAL}[devices.lamp]\nadapter = \"home\"\nmapping = {{}}\n"),
        &environment(),
        &build,
    );
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].location, at(4, 2));
    assert!(errors[0].message.contains("`role-bridge`"), "{}", errors[0]);
}

#[cfg(all(feature = "role-bridge", feature = "adapter-mqtt"))]
mod mqtt {
    //! The `mqtt` kind and the `[devices]` on it.

    use matter_rs_adapter_mqtt::SecretText;
    use matter_rs_bridge::DeviceKind;
    use matter_rs_engine::Secret;

    use super::{MINIMAL, at, environment, errors, errors_with, loaded, single_error, warnings};
    use crate::adapters::AdapterConfig;
    use crate::load::{Build, DeviceMapping};

    /// The two-device configuration of the contract: a dimmable light and a temperature sensor.
    const TWO_DEVICES: &str = "[bridge]

[adapters.home]
kind = \"mqtt\"
url = \"mqtt://broker.local\"

[devices.lamp]
kind = \"dimmable-light\"
adapter = \"home\"
name = \"Lamp\"
mapping = { source = \"hue/lamp\", items = { level = { scale = [0, 100] } } }

[devices.thermometer]
kind = \"temperature-sensor\"
adapter = \"home\"
[devices.thermometer.mapping]
source = \"hue/thermometer\"
items.temperature.factor = 100
";

    #[test]
    fn mqtt_device_config_validates() {
        let loaded = loaded(&format!("{MINIMAL}{TWO_DEVICES}"));
        let config = &loaded.config;
        let AdapterConfig::Mqtt(mqtt) = &config.adapters["home"] else {
            panic!("not an MQTT adapter: {:?}", config.adapters["home"]);
        };
        assert_eq!(mqtt.url, "mqtt://broker.local");
        assert_eq!(mqtt.name, "matter");
        assert_eq!(config.devices.len(), 2);
        let lamp = &config.devices["lamp"];
        assert_eq!(lamp.adapter, "home");
        assert_eq!(lamp.name, "Lamp");
        assert_eq!(lamp.parts.len(), 1);
        assert_eq!(lamp.parts[0].kind, DeviceKind::DimmableLight);
        let DeviceMapping::Mqtt(mapping) = &lamp.mapping;
        assert_eq!(mapping.source, "hue/lamp");
        assert_eq!(mapping.items["level"].scale, Some([0.0, 100.0]));
        let thermometer = &config.devices["thermometer"];
        assert_eq!(thermometer.name, "thermometer");
        let DeviceMapping::Mqtt(mapping) = &thermometer.mapping;
        assert_eq!(mapping.items["temperature"].factor, Some(100.0));

        let definitions = config.mqtt_devices("home");
        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].key.as_str(), "lamp");
        assert_eq!(definitions[1].parts[0].kind, DeviceKind::TemperatureSensor);
        assert!(config.mqtt_devices("other").is_empty());
        assert_eq!(
            loaded.sections,
            [
                "adapters.home",
                "bridge",
                "devices.lamp",
                "devices.thermometer",
                "node"
            ]
        );
        assert!(warnings(&loaded).is_empty());
    }

    #[test]
    fn missing_required_mapping_is_an_error_with_location() {
        // A colour temperature light needs the physical bounds, which have no default item.
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\n\
             [devices.spot]\nkind = \"color-temperature-light\"\nadapter = \"home\"\n\
             [devices.spot.mapping]\nsource = \"hue/spot\"\n"
        );
        let errors = errors(&text);
        assert_eq!(errors.len(), 2, "{errors:#?}");
        for (error, attribute) in errors.iter().zip(["max", "min"]) {
            // Both errors sit at the mapping table and are sorted by their text.
            assert_eq!(error.location, at(12, 1), "{error}");
            assert!(error.message.contains("`mapping.items`"), "{error}");
            assert!(
                error
                    .message
                    .contains(&format!("`color-temperature-{attribute}`")),
                "{error}"
            );
            assert!(error.message.contains("fixed"), "{error}");
        }

        // With the bounds as fixed values the device loads.
        let fixed = format!(
            "{text}items.color-temperature-min.value = 153\nitems.color-temperature-max.value = 500\n"
        );
        assert_eq!(loaded(&fixed).config.devices.len(), 1);
    }

    #[test]
    fn transform_errors_point_at_their_key() {
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\n\
             [devices.lamp]\nkind = \"on-off-light\"\nadapter = \"home\"\n\
             [devices.lamp.mapping]\nsource = \"hue/lamp\"\n\
             [devices.lamp.mapping.items.on-off]\nscale = [0, 1]\n"
        );
        let error = single_error(&text);
        assert_eq!(error.location, at(15, 9), "{error}");
        assert!(error.message.contains("`scale`"), "{error}");
        assert!(error.message.contains("numeric"), "{error}");
    }

    #[test]
    fn mapping_keys_are_checked_against_the_schema() {
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\n\
             [devices.lamp]\nkind = \"on-off-light\"\nadapter = \"home\"\n\
             mapping = {{ topic = \"x\" }}\n"
        );
        let messages: Vec<String> = errors(&text).iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "config.toml:12:11: error: missing required key `source` in \
                 `[devices.lamp.mapping]`",
                "config.toml:12:13: error: unknown key `topic` in `[devices.lamp.mapping]`; \
                 expected one of `source`, `online`, `items`, `parts`",
            ]
        );
    }

    #[test]
    fn devices_need_a_kind_an_adapter_and_a_table() {
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\n\
             [devices.a]\nadapter = \"home\"\nmapping = {{ source = \"x/a\" }}\n\n\
             [devices.b]\nkind = \"toaster\"\nadapter = \"nope\"\nmapping = {{ source = \"x/b\" }}\n\n\
             [devices.c]\nkind = \"on-off-light\"\nparts = [{{ kind = \"on-off-light\" }}]\n\
             adapter = \"home\"\nmapping = {{ source = \"x/c\" }}\n\n\
             [devices.\"D d\"]\nkind = \"on-off-light\"\nadapter = \"home\"\nmapping = {{ source = \"x/d\" }}\n"
        );
        let messages: Vec<String> = errors(&text).iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "config.toml:9:1: error: missing `kind` (or `parts` for a composed device) in \
                 `[devices.a]`",
                "config.toml:14:8: error: `devices.b.kind` is not a device kind; known kinds are \
                 `on-off-light`, `dimmable-light`, `color-temperature-light`, \
                 `extended-color-light`, `on-off-plug-in-unit`, `contact-sensor`, \
                 `temperature-sensor`, `humidity-sensor`, `pressure-sensor`, `light-sensor`, \
                 `occupancy-sensor`, `generic-switch`",
                "config.toml:15:11: error: `devices.b.adapter` names no `[adapters.<id>]` table; \
                 known adapters are `home`",
                "config.toml:20:1: error: `kind` and `parts` exclude each other in \
                 `[devices.c]`; a composed device lists its kinds in `parts` only",
                "config.toml:24:10: error: device id `D d` is not valid; use lower-case letters, \
                 digits, `-` and `_`, starting with a letter or digit",
            ]
        );
    }

    #[test]
    fn composed_devices_load_with_their_parts() {
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\n\
             [devices.strip]\nadapter = \"home\"\n\
             parts = [{{ kind = \"on-off-plug-in-unit\", label = \"Left\" }}, \
             {{ kind = \"on-off-plug-in-unit\", label = \"Right\" }}]\n\
             mapping = {{ source = \"x/strip\", parts = [{{ prefix = \"left\" }}, {{ prefix = \"right\" }}] }}\n"
        );
        let loaded = loaded(&text);
        let strip = &loaded.config.devices["strip"];
        assert_eq!(strip.parts.len(), 2);
        assert_eq!(strip.parts[1].label.as_deref(), Some("Right"));
        assert_eq!(loaded.config.mqtt_devices("home")[0].parts.len(), 2);
    }

    #[test]
    fn mqtt_adapter_rules_have_locations() {
        let text = format!(
            "{MINIMAL}[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtts://broker\"\nname = \"a/b\"\n\
             reconnect = {{ initial = 9, max = 1 }}\n"
        );
        let errors = errors(&text);
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "config.toml:6:7: error: `adapters.home.url`: `mqtts://` needs TLS, which this \
                 build does not include; use `mqtt://`",
                "config.toml:7:8: error: `adapters.home.name`: the instance name must not contain \
                 `/`, `+` or `#`",
                "config.toml:8:13: error: `adapters.home.reconnect`: `initial` must not exceed \
                 `max`",
            ]
        );
    }

    #[test]
    fn mqtt_password_is_resolved_and_never_echoed() {
        let text = format!(
            "{MINIMAL}[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\n\
             password = {{ env = \"MQTT_PW\" }}\n\n\
             [adapters.away]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\npassword = {{ file = \"pw.txt\" }}\n"
        );
        let environment = environment()
            .with_var("MQTT_PW", "from-env")
            .with_file("pw.txt", " from-file\n");
        let loaded = super::load_with_build(&super::source(&text), &environment, &super::build())
            .unwrap_or_else(|error| panic!("{error}"));
        let password = |id: &str| match &loaded.config.adapters[id] {
            AdapterConfig::Mqtt(config) => config.password.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            password("home"),
            Some(SecretText::Plain(Secret::new("from-env".into())))
        );
        assert_eq!(
            password("away"),
            Some(SecretText::Plain(Secret::new("from-file".into())))
        );
        let debug = format!("{:?}", loaded.config);
        assert!(
            !debug.contains("from-env") && !debug.contains("from-file"),
            "{debug}"
        );

        let errors = errors_with(&text, &super::environment(), &super::build());
        let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
        assert_eq!(
            messages,
            [
                "config.toml:7:12: error: `adapters.home.password`: the environment variable \
                 `MQTT_PW` is not set",
                "config.toml:12:12: error: `adapters.away.password`: the secret file `pw.txt` \
                 cannot be read: file not found",
            ]
        );
    }

    #[test]
    fn plain_mqtt_password_in_a_readable_file_warns() {
        let text = format!(
            "{MINIMAL}[adapters.home]\nkind = \"mqtt\"\nurl = \"mqtt://b\"\npassword = \"hunter2\"\n"
        );
        let source = super::ConfigSource {
            mode: Some(0o644),
            ..super::source(&text)
        };
        let loaded = super::load_with_build(&source, &super::environment(), &super::build())
            .unwrap_or_else(|error| panic!("{error}"));
        let warnings = warnings(&loaded);
        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(
            warnings[1].contains("`adapters.home.password`"),
            "{}",
            warnings[1]
        );
        assert!(!warnings[1].contains("hunter2"), "{}", warnings[1]);
    }

    #[test]
    fn test_adapter_feeds_no_devices() {
        let text = format!(
            "{MINIMAL}[bridge]\n[adapters.t]\nkind = \"test\"\ntopic = \"x\"\n\n\
             [devices.lamp]\nkind = \"on-off-light\"\nadapter = \"t\"\nmapping = {{}}\n"
        );
        let error = single_error(&text);
        assert!(error.message.contains("feeds no devices"), "{error}");
        assert_eq!(error.location, at(12, 11));
    }

    #[test]
    fn devices_are_rejected_when_the_role_is_not_built() {
        let build = Build {
            role_bridge: false,
            ..super::build()
        };
        let errors = errors_with(
            &format!("{MINIMAL}{TWO_DEVICES}"),
            &super::environment(),
            &build,
        );
        assert!(
            errors.iter().any(|e| e.message.contains("`role-bridge`")),
            "{errors:#?}"
        );
    }
}

#[test]
fn journald_outside_linux_warns() {
    let build = Build {
        linux: false,
        ..build()
    };
    let text = format!("{MINIMAL}[log]\njournald = true\n");
    let loaded = load_with_build(&source(&text), &environment(), &build).unwrap();
    let warnings = warnings(&loaded);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("config.toml:5:12: warning:"),
        "{warnings:?}"
    );

    let loaded = load_with_build(&source(&text), &environment(), &self::build()).unwrap();
    assert!(loaded.report.diagnostics().is_empty());
}

#[test]
fn both_address_families_off_is_an_error() {
    let error = single_error(&format!("{MINIMAL}[network]\nipv4 = false\nipv6 = false\n"));
    assert_eq!(error.location, at(6, 8));
    assert!(error.message.contains("at least one"), "{error}");
}

#[test]
fn secret_sources_resolve() {
    let plain = loaded(MINIMAL);
    let environment = environment()
        .with_var("PASSCODE", " 13572468\n")
        .with_file("/run/secrets/passcode", "13572468\n")
        .with_file(
            Path::new("/etc/matter-rs").join("relative-code"),
            "13572468",
        );

    let from_env = load_with_build(
        &source("[node]\npasscode = { env = \"PASSCODE\" }\ndiscriminator = 1\n"),
        &environment,
        &build(),
    )
    .unwrap();
    let from_file = load_with_build(
        &source("[node]\npasscode = { file = \"/run/secrets/passcode\" }\ndiscriminator = 1\n"),
        &environment,
        &build(),
    )
    .unwrap();
    let relative_text = "[node]\npasscode = { file = \"relative-code\" }\ndiscriminator = 1\n";
    let from_relative = load_with_build(
        &ConfigSource {
            directory: Some(Path::new("/etc/matter-rs")),
            ..source(relative_text)
        },
        &environment,
        &build(),
    )
    .unwrap();

    let expected = Secret::new(13_572_468_u32);
    assert_eq!(plain.config.node.passcode, expected);
    assert_eq!(from_env.config.node.passcode, expected);
    assert_eq!(from_file.config.node.passcode, expected);
    assert_eq!(from_relative.config.node.passcode, expected);
}

#[test]
fn loaded_config_debug_redacts() {
    let environment = environment().with_var("PASSCODE", "24681357");
    for text in [
        MINIMAL.to_owned(),
        "[node]\npasscode = { env = \"PASSCODE\" }\ndiscriminator = 1\n".to_owned(),
    ] {
        let loaded = load_with_build(&source(&text), &environment, &build()).unwrap();
        let output = format!("{loaded:?} {loaded:#?}");
        assert!(!output.contains("13572468"), "{output}");
        assert!(!output.contains("24681357"), "{output}");
        assert!(output.contains("[REDACTED]"), "{output}");

        let file: crate::ConfigFile = toml::from_str(&text).unwrap();
        let output = format!("{file:?}");
        assert!(!output.contains("13572468"), "{output}");
    }
}

#[test]
fn missing_secret_source_is_error() {
    let error = single_error("[node]\npasscode = { env = \"NOT_SET\" }\ndiscriminator = 1\n");
    assert_eq!(error.location, at(2, 12));
    assert!(error.message.contains("`NOT_SET` is not set"), "{error}");

    let error = single_error("[node]\npasscode = { file = \"/missing\" }\ndiscriminator = 1\n");
    assert_eq!(error.location, at(2, 12));
    assert!(
        error.message.contains("`/missing` cannot be read"),
        "{error}"
    );

    let environment = environment().with_var("PASSCODE", "secret-text");
    let errors = errors_with(
        "[node]\npasscode = { env = \"PASSCODE\" }\ndiscriminator = 1\n",
        &environment,
        &build(),
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].message.contains("does not hold a valid value"));
    assert!(!errors[0].message.contains("secret-text"), "{}", errors[0]);
}

#[test]
fn secret_of_the_wrong_shape_is_rejected_without_echoing_it() {
    let error = single_error("[node]\npasscode = \"letmein\"\ndiscriminator = 1\n");
    assert_eq!(error.location, at(2, 12));
    assert_eq!(
        error.message,
        "`node.passcode` must be an integer, a table with `env` or a table with `file`"
    );

    let error =
        single_error("[node]\npasscode = { env = \"X\", file = \"y\" }\ndiscriminator = 1\n");
    assert!(error.message.contains("must be"), "{error}");

    let error = single_error("[node]\npasscode = -13572468\ndiscriminator = 1\n");
    assert!(!error.message.contains("13572468"), "{error}");
}

#[test]
fn forbidden_passcodes_rejected() {
    for passcode in [
        "0",
        "11111111",
        "22222222",
        "33333333",
        "44444444",
        "55555555",
        "66666666",
        "77777777",
        "88888888",
        "99999999",
        "12345678",
        "87654321",
        "100000000",
    ] {
        let error = single_error(&format!(
            "[node]\npasscode = {passcode}\ndiscriminator = 1\n"
        ));
        assert_eq!(error.location, at(2, 12), "{passcode}");
        assert!(error.message.starts_with("`node.passcode`: "), "{error}");
        assert!(!error.message.contains(passcode), "{error}");
    }
}

#[test]
fn test_passcode_warns() {
    let loaded = loaded("[node]\npasscode = 20202021\ndiscriminator = 3840\n");
    let warnings = warnings(&loaded);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].starts_with("config.toml:2:12: warning: `node.passcode` is the well-known"),
        "{warnings:?}"
    );
    assert!(!warnings[0].contains("20202021"));
}

#[test]
fn plain_secret_in_a_readable_file_warns() {
    let readable = load_with_build(
        &ConfigSource {
            mode: Some(0o100_644),
            ..source(MINIMAL)
        },
        &environment(),
        &build(),
    )
    .unwrap();
    let warnings = warnings(&readable);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("mode 644"), "{warnings:?}");

    let private = load_with_build(
        &ConfigSource {
            mode: Some(0o100_600),
            ..source(MINIMAL)
        },
        &environment(),
        &build(),
    )
    .unwrap();
    assert!(private.report.diagnostics().is_empty());

    let environment = environment().with_var("PASSCODE", "13572468");
    let indirect = load_with_build(
        &ConfigSource {
            mode: Some(0o100_644),
            ..source("[node]\npasscode = { env = \"PASSCODE\" }\ndiscriminator = 1\n")
        },
        &environment,
        &build(),
    )
    .unwrap();
    assert!(indirect.report.diagnostics().is_empty());
}

#[test]
fn relative_state_dir_is_relative_to_the_config_file() {
    let text = format!("{MINIMAL}[state]\ndir = \"state\"\n");
    let loaded = load_with_build(
        &ConfigSource {
            directory: Some(Path::new("/etc/matter-rs")),
            ..source(&text)
        },
        &environment(),
        &build(),
    )
    .unwrap();
    assert_eq!(
        loaded.config.state.dir,
        Path::new("/etc/matter-rs").join("state")
    );
}

#[test]
fn unknown_default_state_dir_is_an_error() {
    let errors = errors_with(MINIMAL, &FakeEnvironment::default(), &build());
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].message.contains("`[state] dir`"), "{}", errors[0]);
}

#[test]
fn damaged_files_never_panic() {
    // A cheap stand-in for the fuzz target in the normal test run: every prefix of a full file,
    // and the file with each line removed, either loads or reports errors.
    let text = format!(
        "{MINIMAL}vendor_name = \"ä name\"\n[network]\nport = 1\n[log]\nlevel = \"info\"\n\
         [bridge]\n[adapters.a]\nkind = \"test\"\ntopic = \"t\"\n[adapters.b]\nkind = \"absent\"\n"
    );
    for end in (0..=text.len()).filter(|end| text.is_char_boundary(*end)) {
        let _ = load_text(&text[..end]);
    }
    let lines: Vec<&str> = text.lines().collect();
    for skip in 0..lines.len() {
        let damaged: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != skip)
            .map(|(_, line)| *line)
            .collect();
        let _ = load_text(&damaged.join("\n"));
    }
}

#[test]
fn integers_in_every_notation_are_checked() {
    let loaded = loaded("[node]\npasscode = 13_572_468\ndiscriminator = 0xFFF\n");
    assert_eq!(loaded.config.node.discriminator, 4095);
    let error = single_error("[node]\npasscode = 13572468\ndiscriminator = 0x1000\n");
    assert!(error.message.contains("between 0 and 4095"), "{error}");
    let error = single_error("[node]\npasscode = 13572468\ndiscriminator = 1.5\n");
    assert!(error.message.contains("found a float"), "{error}");
}
