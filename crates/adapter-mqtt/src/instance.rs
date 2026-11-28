//! The bridge as an mqtt-smarthome instance: its `info` document and the time stamps.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// The specification version the instance implements.
pub const SPEC_VERSION: &str = "2.0";
/// The adapter name in `info`.
pub const ADAPTER_NAME: &str = "matter-rs";

/// The Matter identity of the node, published in `info` under `matter`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeIdentity {
    /// The vendor id the node announces.
    pub vendor_id: u16,
    /// The product id the node announces.
    pub product_id: u16,
}

/// Builds the retained `<name>/info` document (specification section 6).
#[must_use]
pub fn info_document(
    version: &str,
    host: Option<&str>,
    pid: u32,
    started: SystemTime,
    identity: Option<BridgeIdentity>,
) -> String {
    let mut info = Map::new();
    info.insert(String::from("name"), json!(ADAPTER_NAME));
    info.insert(String::from("version"), json!(version));
    info.insert(String::from("spec"), json!(SPEC_VERSION));
    if let Some(host) = host {
        info.insert(String::from("host"), json!(host));
    }
    info.insert(String::from("pid"), json!(pid));
    info.insert(String::from("started"), json!(iso8601(started)));
    info.insert(String::from("maintenance"), json!(false));
    if let Some(identity) = identity {
        info.insert(
            String::from("matter"),
            json!({ "vendor_id": identity.vendor_id, "product_id": identity.product_id }),
        );
    }
    Value::Object(info).to_string()
}

/// Milliseconds since the Unix epoch, for `ts` and `lc`.
#[must_use]
pub fn now_millis() -> u64 {
    millis(SystemTime::now())
}

fn millis(time: SystemTime) -> u64 {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

/// A time as ISO 8601 in UTC with second precision, `2026-11-28T10:15:30Z`.
#[must_use]
pub fn iso8601(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date of a day count since 1970-01-01 (Howard Hinnant's algorithm,
/// <https://howardhinnant.github.io/date_algorithms.html>).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    // Month and day are within 1..=12 and 1..=31 by construction.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    (year, month as u32, day as u32)
}

/// The host name, for `info`: from the environment (`HOSTNAME`, `COMPUTERNAME`) or the
/// `/etc/hostname` file; `None` when none of them says.
#[must_use]
pub fn host_name() -> Option<String> {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_owned())
        .find(|value| !value.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use serde_json::Value;

    use super::{BridgeIdentity, info_document, iso8601};

    #[test]
    fn iso8601_formats_known_instants() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        let time = UNIX_EPOCH + Duration::from_secs(1_421_792_677);
        assert_eq!(iso8601(time), "2015-01-20T22:24:37Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(iso8601(leap), "2000-02-29T00:00:00Z");
        let late = UNIX_EPOCH + Duration::from_secs(4_102_444_799);
        assert_eq!(iso8601(late), "2099-12-31T23:59:59Z");
    }

    #[test]
    fn info_has_the_fields_of_section_six() {
        let text = info_document(
            "0.1.0",
            Some("bridge-host"),
            4242,
            UNIX_EPOCH + Duration::from_secs(86_400),
            Some(BridgeIdentity {
                vendor_id: 0xFFF1,
                product_id: 0x8001,
            }),
        );
        let info: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(info["name"], "matter-rs");
        assert_eq!(info["version"], "0.1.0");
        assert_eq!(info["spec"], "2.0");
        assert_eq!(info["host"], "bridge-host");
        assert_eq!(info["pid"], 4242);
        assert_eq!(info["started"], "1970-01-02T00:00:00Z");
        assert_eq!(info["maintenance"], false);
        assert_eq!(info["matter"]["vendor_id"], 0xFFF1);
        assert_eq!(info["matter"]["product_id"], 0x8001);

        let bare: Value =
            serde_json::from_str(&info_document("0.1.0", None, 1, UNIX_EPOCH, None)).unwrap();
        assert!(bare.get("host").is_none());
        assert!(bare.get("matter").is_none());
    }
}
