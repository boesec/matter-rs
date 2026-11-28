//! MQTT payloads as mqtt-smarthome defines them, parsed tolerantly and encoded for publishing.
//!
//! A status payload is a plain value (the text of a boolean, a number or a string) or a JSON
//! object with a `val` key (specification section 5). Anything a broker delivers is accepted
//! here without a panic: invalid UTF-8 is replaced, text that is not JSON is a string, and the
//! conversions of section 5.3 (`on`/`off`, `1`/`0`, numeric text) are applied when a boolean or
//! a number is wanted.

use serde_json::{Map, Number, Value};

/// Parses the bytes of a message into a JSON value; `None` for an empty payload, which means
/// "no value" (and clears a retained topic).
///
/// Text that is not valid JSON is a plain string (plain strings are written without quotes).
#[must_use]
pub fn parse(bytes: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_owned())))
}

/// Unwraps the mqtt-smarthome object form: an object with a `val` key yields that value, any
/// other value is returned as it is.
#[must_use]
pub fn unwrap_val(value: Value) -> Value {
    match value {
        Value::Object(mut object) if object.contains_key("val") => {
            object.remove("val").unwrap_or(Value::Null)
        }
        other => other,
    }
}

/// Whether a payload is in the object form (an object with a `val` key).
#[must_use]
pub fn is_object_form(value: &Value) -> bool {
    matches!(value, Value::Object(object) if object.contains_key("val"))
}

/// A boolean from a value, with the conversions of section 5.3: `true`/`false`, `1`/`0`,
/// `on`/`off`, `yes`/`no`, case-insensitive; numbers `1` and `0`.
#[must_use]
pub fn to_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => {
            let n = number.as_f64()?;
            if n == 1.0 {
                Some(true)
            } else if n == 0.0 {
                Some(false)
            } else {
                None
            }
        }
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "on" | "yes" | "1" => Some(true),
            "false" | "off" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// A number from a value: a JSON number, numeric text, or a boolean as `1`/`0`.
#[must_use]
pub fn to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64().filter(|n| n.is_finite()),
        Value::String(text) => text.trim().parse::<f64>().ok().filter(|n| n.is_finite()),
        Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// The text of a scalar for comparisons with the keys of a `map` transform: booleans and numbers
/// as JSON writes them, strings as they are. `None` for arrays, objects and null.
#[must_use]
pub fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::Bool(_) | Value::Number(_) => Some(value.to_string()),
        Value::String(text) => Some(text.clone()),
        Value::Null | Value::Array(_) | Value::Object(_) => None,
    }
}

/// A JSON number from a float: an integer when the float is integral and fits.
#[must_use]
pub fn number(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        // The checks above make the cast exact.
        #[allow(clippy::cast_possible_truncation)]
        return Value::Number(Number::from(value as i64));
    }
    Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// Encodes a value for a plain `set` or `status` message: booleans and numbers as their text,
/// strings without quotes, anything else as JSON.
#[must_use]
pub fn encode_plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Encodes a value as the object form `{"val": ...}`.
#[must_use]
pub fn encode_val(value: Value) -> String {
    let mut object = Map::new();
    object.insert(String::from("val"), value);
    Value::Object(object).to_string()
}

/// Encodes a status value as `{"val": ..., "ts": ..., "lc": ...}` with milliseconds since the
/// epoch.
#[must_use]
pub fn encode_status(value: Value, ts: u64, lc: u64) -> String {
    let mut object = Map::new();
    object.insert(String::from("val"), value);
    object.insert(String::from("ts"), Value::from(ts));
    object.insert(String::from("lc"), Value::from(lc));
    Value::Object(object).to_string()
}

/// Nests `value` under a JSON pointer (`/state/on` gives `{"state": {"on": value}}`), the inverse
/// of looking the pointer up. Array indexes are not supported for building and become object
/// keys. An empty pointer is the value itself.
#[must_use]
pub fn nest(pointer: &str, value: Value) -> Value {
    let mut current = value;
    let keys: Vec<&str> = pointer.split('/').skip(1).collect();
    for key in keys.into_iter().rev() {
        let key = key.replace("~1", "/").replace("~0", "~");
        let mut object = Map::new();
        object.insert(key, current);
        current = Value::Object(object);
    }
    current
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        encode_plain, encode_status, encode_val, is_object_form, nest, number, parse, scalar_text,
        to_bool, to_f64, unwrap_val,
    };

    #[test]
    fn plain_values_parse_as_their_json_type_or_as_text() {
        assert_eq!(parse(b"true"), Some(Value::Bool(true)));
        assert_eq!(parse(b" 12 \n"), Some(json!(12)));
        assert_eq!(parse(b"-3.5"), Some(json!(-3.5)));
        assert_eq!(parse(b"on"), Some(json!("on")));
        assert_eq!(parse(b"\"quoted\""), Some(json!("quoted")));
        assert_eq!(parse(b"[1,2]"), Some(json!([1, 2])));
        assert_eq!(parse(b""), None);
        assert_eq!(parse(b"   "), None);
        assert_eq!(parse(&[0xff, 0xfe]), Some(json!("\u{fffd}\u{fffd}")));
    }

    #[test]
    fn object_form_is_unwrapped_and_other_fields_ignored() {
        let value = parse(br#"{"val": 17.9, "ts": 1, "lc": 1, "hm": {"x": 1}}"#).unwrap();
        assert!(is_object_form(&value));
        assert_eq!(unwrap_val(value), json!(17.9));
        let other = json!({"state": {"on": true}});
        assert!(!is_object_form(&other));
        assert_eq!(unwrap_val(other.clone()), other);
    }

    #[test]
    fn booleans_follow_the_conversions_of_the_specification() {
        for text in ["true", "TRUE", "on", "On", "yes", "1"] {
            assert_eq!(to_bool(&json!(text)), Some(true), "{text}");
        }
        for text in ["false", "off", "no", "0"] {
            assert_eq!(to_bool(&json!(text)), Some(false), "{text}");
        }
        assert_eq!(to_bool(&json!(1)), Some(true));
        assert_eq!(to_bool(&json!(0.0)), Some(false));
        assert_eq!(to_bool(&json!(2)), None);
        assert_eq!(to_bool(&json!("maybe")), None);
        assert_eq!(to_bool(&json!(null)), None);
        assert_eq!(to_bool(&json!([true])), None);
    }

    #[test]
    fn numbers_come_from_numbers_text_and_booleans() {
        assert_eq!(to_f64(&json!(21.5)), Some(21.5));
        assert_eq!(to_f64(&json!(" 42 ")), Some(42.0));
        assert_eq!(to_f64(&json!(true)), Some(1.0));
        assert_eq!(to_f64(&json!("abc")), None);
        assert_eq!(to_f64(&json!("inf")), None);
        assert_eq!(to_f64(&json!({})), None);
    }

    #[test]
    fn scalars_have_a_text_and_numbers_prefer_integers() {
        assert_eq!(scalar_text(&json!(true)), Some("true".into()));
        assert_eq!(scalar_text(&json!(1.5)), Some("1.5".into()));
        assert_eq!(scalar_text(&json!("ON")), Some("ON".into()));
        assert_eq!(scalar_text(&json!(null)), None);
        assert_eq!(number(3.0), json!(3));
        assert_eq!(number(-3.25), json!(-3.25));
        assert_eq!(number(f64::NAN), Value::Null);
    }

    #[test]
    fn encodings_match_the_specification() {
        assert_eq!(encode_plain(&json!(true)), "true");
        assert_eq!(encode_plain(&json!(100)), "100");
        assert_eq!(encode_plain(&json!("Lamp")), "Lamp");
        assert_eq!(encode_plain(&json!([1])), "[1]");
        assert_eq!(encode_val(json!(true)), r#"{"val":true}"#);
        assert_eq!(
            encode_status(json!(2), 1000, 500),
            r#"{"val":2,"ts":1000,"lc":500}"#
        );
    }

    #[test]
    fn nest_is_the_inverse_of_pointer_lookup() {
        let nested = nest("/state/on", json!(true));
        assert_eq!(nested, json!({"state": {"on": true}}));
        assert_eq!(nested.pointer("/state/on"), Some(&json!(true)));
        assert_eq!(nest("", json!(5)), json!(5));
        assert_eq!(nest("/a~1b", json!(1)), json!({"a/b": 1}));
    }

    #[test]
    fn damaged_payloads_never_panic() {
        let samples: [&[u8]; 12] = [
            b"",
            b"{",
            b"}",
            b"{\"val\":",
            b"[",
            b"\xff\xfe\x00",
            b"nul\0l",
            b"1e999999",
            b"-",
            b"{\"val\": {\"val\": {\"val\": 1}}}",
            b"\"\\u0000\"",
            &[b'9'; 4096],
        ];
        for bytes in samples {
            if let Some(value) = parse(bytes) {
                let _ = to_bool(&value);
                let _ = to_f64(&value);
                let _ = scalar_text(&value);
                let _ = encode_plain(&value);
                let _ = unwrap_val(value);
            }
        }
    }
}
