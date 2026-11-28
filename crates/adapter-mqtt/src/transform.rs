//! The fixed set of transforms between an MQTT value and a typed attribute value, and their
//! inverses for `set` messages.
//!
//! A mapping may name one of these per item (combinations below). Inbound, a payload goes
//! through: `path` (or the `val` unwrapping), then `map`, then `factor` or `scale`, then
//! `invert`, then rounding and clamping to the attribute's range. Outbound, the same steps run
//! backwards, so that a `set` carries the value in the source's own terms.
//!
//! | Transform | Attribute type | Meaning |
//! |---|---|---|
//! | `path` | any | A JSON pointer into the payload (`/state/on`); replaces the `val` unwrapping |
//! | `map` | any | Payload text to attribute value (`{ "ON" = true }`), case-insensitive; the inverse publishes the key |
//! | `factor` | number | Attribute value = payload × factor (`100` turns degrees into hundredths) |
//! | `scale` | number | `[min, max]` of the payload mapped linearly onto the attribute's range |
//! | `invert` | boolean, or number with `scale` | Negates a boolean; reverses the direction of a scale |
//!
//! `map` excludes `factor` and `scale`; `factor` excludes `scale`. Everything else that does not
//! fit the attribute's type is a configuration error, reported by [`Transform::validate`].

use core::fmt;

use matter_rs_engine::Value;
use serde_json::Value as Json;

use crate::payload;

/// The integer width of an attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntKind {
    /// An unsigned 8-bit attribute.
    U8,
    /// An unsigned 16-bit attribute.
    U16,
    /// A signed 16-bit attribute.
    I16,
}

/// The type of a mappable attribute: what a payload must become.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    /// A boolean attribute.
    Bool,
    /// An integer attribute with its range; `nullable` attributes take null for "no value".
    Integer {
        /// The width.
        kind: IntKind,
        /// The smallest valid value.
        min: i64,
        /// The largest valid value.
        max: i64,
        /// Whether null is a valid value.
        nullable: bool,
    },
}

impl ValueType {
    /// Whether null (and an empty payload) is a valid value.
    #[must_use]
    pub const fn is_nullable(self) -> bool {
        match self {
            Self::Bool => false,
            Self::Integer { nullable, .. } => nullable,
        }
    }

    /// The range of an integer type; `None` for a boolean.
    #[must_use]
    pub const fn range(self) -> Option<(i64, i64)> {
        match self {
            Self::Bool => None,
            Self::Integer { min, max, .. } => Some((min, max)),
        }
    }

    /// An attribute value from a number: rounded half away from zero and clamped to the range.
    #[must_use]
    pub fn integer(self, number: f64) -> Value {
        let Self::Integer { kind, min, max, .. } = self else {
            return Value::Bool(number != 0.0);
        };
        let rounded = number.round().clamp(min as f64, max as f64);
        // Within `min..=max`, which fits the attribute's width by construction.
        #[allow(clippy::cast_possible_truncation)]
        let n = rounded as i64;
        match kind {
            IntKind::U8 => Value::U8(u8::try_from(n).unwrap_or(u8::MAX)),
            IntKind::U16 => Value::U16(u16::try_from(n).unwrap_or(u16::MAX)),
            IntKind::I16 => Value::I16(i16::try_from(n).unwrap_or(i16::MAX)),
        }
    }

    /// The number of an attribute value of this type; `None` for null or a mismatch.
    #[must_use]
    pub fn number_of(self, value: &Value) -> Option<f64> {
        match value {
            Value::Bool(flag) => Some(if *flag { 1.0 } else { 0.0 }),
            Value::U8(n) => Some(f64::from(*n)),
            Value::U16(n) => Some(f64::from(*n)),
            Value::I16(n) => Some(f64::from(*n)),
            Value::U32(n) => Some(f64::from(*n)),
            Value::I32(n) => Some(f64::from(*n)),
            Value::I8(n) => Some(f64::from(*n)),
            _ => None,
        }
    }

    /// A fixed value from the configuration (`value = ...`) as an attribute value.
    ///
    /// # Errors
    ///
    /// A message when the value does not fit the type.
    pub fn constant(self, json: &Json) -> Result<Value, String> {
        match self {
            Self::Bool => payload::to_bool(json)
                .map(Value::Bool)
                .ok_or_else(|| String::from("must be a boolean")),
            Self::Integer {
                min, max, nullable, ..
            } => {
                if json.is_null() {
                    return if nullable {
                        Ok(Value::Null)
                    } else {
                        Err(String::from("must not be null"))
                    };
                }
                let number =
                    payload::to_f64(json).ok_or_else(|| String::from("must be a number"))?;
                if number.fract() != 0.0 || number < min as f64 || number > max as f64 {
                    return Err(format!("must be an integer from {min} to {max}"));
                }
                Ok(self.integer(number))
            }
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("a boolean"),
            Self::Integer {
                min, max, nullable, ..
            } => {
                write!(f, "an integer from {min} to {max}")?;
                if *nullable {
                    f.write_str(" or null")?;
                }
                Ok(())
            }
        }
    }
}

/// The payload range of a `scale` transform.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    /// The payload value that maps onto the attribute's minimum (or maximum with `invert`).
    pub min: f64,
    /// The payload value that maps onto the attribute's maximum (or minimum with `invert`).
    pub max: f64,
}

/// The transforms of one item; see the [module](self) documentation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transform {
    /// A JSON pointer into the payload.
    pub path: Option<String>,
    /// Payload text to attribute value, in configuration order.
    pub map: Vec<(String, Json)>,
    /// Attribute value = payload × factor.
    pub factor: Option<f64>,
    /// The payload range mapped onto the attribute's range.
    pub scale: Option<Scale>,
    /// Negate a boolean, or reverse the direction of the scale.
    pub invert: bool,
}

/// Why an inbound payload produced no attribute value. Logged at debug level: a payload the
/// mapping does not understand is normal operation, not an error of the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rejected {
    /// The payload is empty and the attribute is not nullable.
    Empty,
    /// The JSON pointer names nothing in the payload.
    MissingPath,
    /// The value is null and the attribute is not nullable.
    Null,
    /// The value is not a boolean in any accepted spelling.
    NotBoolean,
    /// The value is not a number in any accepted spelling.
    NotNumber,
    /// The value matches no key of the `map`.
    Unmapped,
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Empty => "empty payload for an attribute that cannot be null",
            Self::MissingPath => "the JSON pointer names nothing in the payload",
            Self::Null => "null for an attribute that cannot be null",
            Self::NotBoolean => "not a boolean",
            Self::NotNumber => "not a number",
            Self::Unmapped => "no entry of `map` matches",
        })
    }
}

impl Transform {
    /// Checks that the transforms fit the attribute's type and each other.
    ///
    /// # Errors
    ///
    /// A message that names the offending key, for the configuration report.
    pub fn validate(&self, ty: ValueType) -> Result<(), String> {
        if let Some(path) = &self.path
            && !(path.is_empty() || path.starts_with('/'))
        {
            return Err(String::from(
                "`path` must be a JSON pointer starting with `/`, for example `/state/on`",
            ));
        }
        if !self.map.is_empty() && (self.factor.is_some() || self.scale.is_some()) {
            return Err(String::from(
                "`map` cannot be combined with `factor` or `scale`",
            ));
        }
        if self.factor.is_some() && self.scale.is_some() {
            return Err(String::from("`factor` and `scale` exclude each other"));
        }
        for (key, value) in &self.map {
            if key.is_empty() {
                return Err(String::from("`map` keys must not be empty"));
            }
            ty.constant(value)
                .map_err(|reason| format!("`map` value of `{key}` {reason}"))?;
        }
        match ty {
            ValueType::Bool => {
                if self.factor.is_some() {
                    return Err(String::from("`factor` needs a numeric attribute"));
                }
                if self.scale.is_some() {
                    return Err(String::from("`scale` needs a numeric attribute"));
                }
            }
            ValueType::Integer { .. } => {
                if let Some(factor) = self.factor
                    && !(factor.is_finite() && factor != 0.0)
                {
                    return Err(String::from(
                        "`factor` must be a finite number other than 0",
                    ));
                }
                if let Some(scale) = self.scale
                    && !(scale.min.is_finite() && scale.max.is_finite() && scale.min != scale.max)
                {
                    return Err(String::from(
                        "`scale` needs two different finite numbers, `[min, max]`",
                    ));
                }
                if self.invert && self.scale.is_none() {
                    return Err(String::from(
                        "`invert` on a numeric attribute needs a `scale` whose direction it reverses",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Turns a payload into an attribute value of type `ty`; `None` is an empty payload.
    ///
    /// # Errors
    ///
    /// [`Rejected`] when the payload carries no value of that type.
    pub fn inbound(&self, ty: ValueType, payload: Option<Json>) -> Result<Value, Rejected> {
        let Some(payload) = payload else {
            return if ty.is_nullable() {
                Ok(Value::Null)
            } else {
                Err(Rejected::Empty)
            };
        };
        let value = match &self.path {
            Some(pointer) => payload
                .pointer(pointer)
                .cloned()
                .ok_or(Rejected::MissingPath)?,
            None => payload::unwrap_val(payload),
        };
        if value.is_null() {
            return if ty.is_nullable() {
                Ok(Value::Null)
            } else {
                Err(Rejected::Null)
            };
        }
        let value = if self.map.is_empty() {
            value
        } else {
            let text = payload::scalar_text(&value).ok_or(Rejected::Unmapped)?;
            self.map
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(&text))
                .map(|(_, mapped)| mapped.clone())
                .ok_or(Rejected::Unmapped)?
        };
        match ty {
            ValueType::Bool => {
                let flag = payload::to_bool(&value).ok_or(Rejected::NotBoolean)?;
                Ok(Value::Bool(flag ^ self.invert))
            }
            ValueType::Integer { min, max, .. } => {
                let mut number = payload::to_f64(&value).ok_or(Rejected::NotNumber)?;
                if let Some(factor) = self.factor {
                    number *= factor;
                }
                if let Some(scale) = self.scale {
                    let position = if self.invert {
                        (scale.max - number) / (scale.max - scale.min)
                    } else {
                        (number - scale.min) / (scale.max - scale.min)
                    };
                    number = min as f64 + position * (max - min) as f64;
                }
                Ok(ty.integer(number))
            }
        }
    }

    /// Turns an attribute value into the wire value of a `set` message (before the object form
    /// is applied); `None` when the value has no representation under this mapping.
    #[must_use]
    pub fn outbound(&self, ty: ValueType, value: &Value) -> Option<Json> {
        let wire = match ty {
            ValueType::Bool => {
                let flag = bool::try_from(value.clone()).ok()? ^ self.invert;
                self.map_key(ty, &Value::Bool(flag))
                    .unwrap_or(Json::Bool(flag))
            }
            ValueType::Integer { min, max, .. } => {
                if let Some(key) = self.map_key(ty, value) {
                    key
                } else {
                    let mut number = ty.number_of(value)?;
                    if let Some(factor) = self.factor {
                        number /= factor;
                    }
                    if let Some(scale) = self.scale {
                        let position = (number - min as f64) / (max - min) as f64;
                        number = if self.invert {
                            scale.max - position * (scale.max - scale.min)
                        } else {
                            scale.min + position * (scale.max - scale.min)
                        };
                        if scale.min.fract() == 0.0 && scale.max.fract() == 0.0 {
                            number = number.round();
                        }
                    }
                    payload::number(number)
                }
            }
        };
        Some(match &self.path {
            Some(pointer) => payload::nest(pointer, wire),
            None => wire,
        })
    }

    /// The first `map` key whose value is `value` (read as the attribute's type, like
    /// [`validate`](Self::validate) reads it), as the wire value.
    fn map_key(&self, ty: ValueType, value: &Value) -> Option<Json> {
        self.map
            .iter()
            .find(|(_, mapped)| ty.constant(mapped).ok().as_ref() == Some(value))
            .map(|(key, _)| Json::String(key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::Value;
    use proptest::prelude::*;
    use serde_json::json;

    use super::{IntKind, Rejected, Scale, Transform, ValueType};

    const LEVEL: ValueType = ValueType::Integer {
        kind: IntKind::U8,
        min: 1,
        max: 254,
        nullable: true,
    };
    const TEMPERATURE: ValueType = ValueType::Integer {
        kind: IntKind::I16,
        min: -27315,
        max: 32767,
        nullable: true,
    };
    const OCCUPANCY: ValueType = ValueType::Integer {
        kind: IntKind::U8,
        min: 0,
        max: 1,
        nullable: false,
    };

    fn scale(min: f64, max: f64) -> Transform {
        Transform {
            scale: Some(Scale { min, max }),
            ..Transform::default()
        }
    }

    #[test]
    fn identity_rounds_and_clamps() {
        let identity = Transform::default();
        assert_eq!(
            identity.inbound(LEVEL, Some(json!(128))),
            Ok(Value::U8(128))
        );
        assert_eq!(
            identity.inbound(LEVEL, Some(json!("77"))),
            Ok(Value::U8(77))
        );
        assert_eq!(identity.inbound(LEVEL, Some(json!(0))), Ok(Value::U8(1)));
        assert_eq!(
            identity.inbound(LEVEL, Some(json!(999))),
            Ok(Value::U8(254))
        );
        assert_eq!(identity.inbound(LEVEL, Some(json!(2.5))), Ok(Value::U8(3)));
        assert_eq!(identity.inbound(LEVEL, None), Ok(Value::Null));
        assert_eq!(identity.inbound(LEVEL, Some(json!(null))), Ok(Value::Null));
        assert_eq!(identity.inbound(OCCUPANCY, None), Err(Rejected::Empty));
        assert_eq!(
            identity.inbound(OCCUPANCY, Some(json!(null))),
            Err(Rejected::Null)
        );
        assert_eq!(
            identity.inbound(OCCUPANCY, Some(json!(true))),
            Ok(Value::U8(1))
        );
        assert_eq!(
            identity.inbound(LEVEL, Some(json!("x"))),
            Err(Rejected::NotNumber)
        );
        assert_eq!(
            identity.inbound(ValueType::Bool, Some(json!("ON"))),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            identity.inbound(
                ValueType::Bool,
                Some(json!({"val": "off", "ts": 1, "lc": 1}))
            ),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            identity.inbound(ValueType::Bool, Some(json!(3))),
            Err(Rejected::NotBoolean)
        );
        assert_eq!(
            identity.outbound(ValueType::Bool, &Value::Bool(true)),
            Some(json!(true))
        );
        assert_eq!(identity.outbound(LEVEL, &Value::U8(5)), Some(json!(5)));
        assert_eq!(identity.outbound(LEVEL, &Value::Null), None);
    }

    #[test]
    fn path_replaces_the_val_unwrapping() {
        let transform = Transform {
            path: Some(String::from("/state/on")),
            ..Transform::default()
        };
        let payload = json!({"state": {"on": true}, "val": false});
        assert_eq!(
            transform.inbound(ValueType::Bool, Some(payload)),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            transform.inbound(ValueType::Bool, Some(json!({"val": true}))),
            Err(Rejected::MissingPath)
        );
        assert_eq!(
            transform.outbound(ValueType::Bool, &Value::Bool(false)),
            Some(json!({"state": {"on": false}}))
        );
    }

    #[test]
    fn map_matches_case_insensitively_and_inverts_to_the_key() {
        let transform = Transform {
            map: vec![
                (String::from("ON"), json!(true)),
                (String::from("OFF"), json!(false)),
            ],
            ..Transform::default()
        };
        assert!(transform.validate(ValueType::Bool).is_ok());
        assert_eq!(
            transform.inbound(ValueType::Bool, Some(json!("on"))),
            Ok(Value::Bool(true))
        );
        assert_eq!(
            transform.inbound(ValueType::Bool, Some(json!("dim"))),
            Err(Rejected::Unmapped)
        );
        assert_eq!(
            transform.outbound(ValueType::Bool, &Value::Bool(false)),
            Some(json!("OFF"))
        );
        let numeric = Transform {
            map: vec![
                (String::from("low"), json!(1)),
                (String::from("high"), json!(254)),
            ],
            ..Transform::default()
        };
        assert_eq!(
            numeric.inbound(LEVEL, Some(json!("HIGH"))),
            Ok(Value::U8(254))
        );
        assert_eq!(numeric.outbound(LEVEL, &Value::U8(1)), Some(json!("low")));
        assert_eq!(numeric.outbound(LEVEL, &Value::U8(7)), Some(json!(7)));
    }

    #[test]
    fn factor_and_scale_convert_both_ways() {
        let factor = Transform {
            factor: Some(100.0),
            ..Transform::default()
        };
        assert_eq!(
            factor.inbound(TEMPERATURE, Some(json!(21.5))),
            Ok(Value::I16(2150))
        );
        assert_eq!(
            factor.inbound(TEMPERATURE, Some(json!({"val": 22.25}))),
            Ok(Value::I16(2225))
        );
        assert_eq!(
            factor.outbound(TEMPERATURE, &Value::I16(2150)),
            Some(json!(21.5))
        );

        let percent = scale(0.0, 100.0);
        assert_eq!(percent.inbound(LEVEL, Some(json!(0))), Ok(Value::U8(1)));
        assert_eq!(percent.inbound(LEVEL, Some(json!(100))), Ok(Value::U8(254)));
        assert_eq!(percent.inbound(LEVEL, Some(json!(50))), Ok(Value::U8(128)));
        assert_eq!(percent.inbound(LEVEL, Some(json!(150))), Ok(Value::U8(254)));
        assert_eq!(percent.outbound(LEVEL, &Value::U8(254)), Some(json!(100)));
        assert_eq!(percent.outbound(LEVEL, &Value::U8(1)), Some(json!(0)));

        let inverted = Transform {
            invert: true,
            ..scale(0.0, 100.0)
        };
        assert_eq!(inverted.inbound(LEVEL, Some(json!(0))), Ok(Value::U8(254)));
        assert_eq!(inverted.outbound(LEVEL, &Value::U8(254)), Some(json!(0)));

        let fraction = scale(0.0, 1.0);
        assert_eq!(
            fraction.inbound(LEVEL, Some(json!(0.5))),
            Ok(Value::U8(128))
        );
        let out = fraction.outbound(LEVEL, &Value::U8(254)).unwrap();
        assert_eq!(out, json!(1));
    }

    #[test]
    fn invert_negates_booleans() {
        let transform = Transform {
            invert: true,
            ..Transform::default()
        };
        assert_eq!(
            transform.inbound(ValueType::Bool, Some(json!(true))),
            Ok(Value::Bool(false))
        );
        assert_eq!(
            transform.outbound(ValueType::Bool, &Value::Bool(false)),
            Some(json!(true))
        );
    }

    #[test]
    fn validation_rejects_transforms_that_do_not_fit() {
        let scale_on_bool = scale(0.0, 1.0);
        assert!(
            scale_on_bool
                .validate(ValueType::Bool)
                .unwrap_err()
                .contains("scale")
        );
        let factor_on_bool = Transform {
            factor: Some(2.0),
            ..Transform::default()
        };
        assert!(
            factor_on_bool
                .validate(ValueType::Bool)
                .unwrap_err()
                .contains("factor")
        );
        let invert_number = Transform {
            invert: true,
            ..Transform::default()
        };
        assert!(
            invert_number
                .validate(LEVEL)
                .unwrap_err()
                .contains("invert")
        );
        let both = Transform {
            factor: Some(2.0),
            ..scale(0.0, 1.0)
        };
        assert!(both.validate(LEVEL).unwrap_err().contains("exclude"));
        let map_and_factor = Transform {
            map: vec![(String::from("a"), json!(1))],
            factor: Some(2.0),
            ..Transform::default()
        };
        assert!(map_and_factor.validate(LEVEL).unwrap_err().contains("map"));
        let bad_map = Transform {
            map: vec![(String::from("a"), json!("x"))],
            ..Transform::default()
        };
        assert!(bad_map.validate(LEVEL).unwrap_err().contains("`a`"));
        let bad_path = Transform {
            path: Some(String::from("state.on")),
            ..Transform::default()
        };
        assert!(bad_path.validate(LEVEL).unwrap_err().contains("path"));
        assert!(scale(1.0, 1.0).validate(LEVEL).is_err());
        let zero = Transform {
            factor: Some(0.0),
            ..Transform::default()
        };
        assert!(zero.validate(LEVEL).is_err());
        assert!(scale(0.0, 100.0).validate(LEVEL).is_ok());
    }

    #[test]
    fn constants_are_type_checked() {
        assert_eq!(
            ValueType::Bool.constant(&json!("on")),
            Ok(Value::Bool(true))
        );
        assert_eq!(LEVEL.constant(&json!(10)), Ok(Value::U8(10)));
        assert_eq!(LEVEL.constant(&json!(null)), Ok(Value::Null));
        assert!(LEVEL.constant(&json!(300)).is_err());
        assert!(LEVEL.constant(&json!(1.5)).is_err());
        assert!(OCCUPANCY.constant(&json!(null)).is_err());
        assert!(ValueType::Bool.constant(&json!(7)).is_err());
        assert_eq!(LEVEL.to_string(), "an integer from 1 to 254 or null");
    }

    #[test]
    fn damaged_payloads_never_panic() {
        let transforms = [
            Transform::default(),
            scale(0.0, 100.0),
            Transform {
                factor: Some(1e300),
                ..Transform::default()
            },
            Transform {
                path: Some(String::from("/a/b")),
                ..Transform::default()
            },
            Transform {
                map: vec![(String::from("x"), json!(1))],
                ..Transform::default()
            },
        ];
        let payloads = [
            json!(1e308),
            json!(-1e308),
            json!("1e999"),
            json!("NaN"),
            json!([[[]]]),
            json!({"val": {"val": null}}),
            json!({"a": [1, 2]}),
            json!(""),
            json!(f64::MAX),
        ];
        for transform in &transforms {
            for ty in [ValueType::Bool, LEVEL, TEMPERATURE, OCCUPANCY] {
                for payload in &payloads {
                    let _ = transform.inbound(ty, Some(payload.clone()));
                }
                let _ = transform.outbound(ty, &Value::U8(200));
                let _ = transform.outbound(ty, &Value::I16(-30000));
                let _ = transform.outbound(ty, &Value::Utf8(String::from("x")));
            }
        }
    }

    fn integer_types() -> impl Strategy<Value = ValueType> {
        prop_oneof![Just(LEVEL), Just(TEMPERATURE), Just(OCCUPANCY)]
    }

    proptest! {
        /// Every invertible transform gives the attribute value back after a round trip through
        /// the wire value: factors, scales at least as fine as the attribute's range, booleans
        /// with `invert`, and maps.
        #[test]
        fn transforms_round_trip(
            ty in integer_types(),
            seed in 0_i64..=70_000,
            factor in prop_oneof![Just(1.0), Just(10.0), Just(100.0), Just(1000.0)],
            width_extra in 0_i64..=100_000,
            invert in any::<bool>(),
            flag in any::<bool>(),
        ) {
            let (min, max) = ty.range().unwrap();
            let value = ty.integer((min + seed.rem_euclid(max - min + 1)) as f64);

            let by_factor = Transform { factor: Some(factor), ..Transform::default() };
            let wire = by_factor.outbound(ty, &value).unwrap();
            prop_assert_eq!(by_factor.inbound(ty, Some(wire)), Ok(value.clone()));

            let width = (max - min) + width_extra;
            let by_scale = Transform {
                scale: Some(Scale { min: 0.0, max: width as f64 }),
                invert,
                ..Transform::default()
            };
            let wire = by_scale.outbound(ty, &value).unwrap();
            prop_assert_eq!(by_scale.inbound(ty, Some(wire)), Ok(value.clone()));

            let by_map = Transform {
                map: vec![
                    (String::from("lo"), json!(min)),
                    (String::from("hi"), json!(max)),
                ],
                ..Transform::default()
            };
            for edge in [ty.integer(min as f64), ty.integer(max as f64)] {
                let wire = by_map.outbound(ty, &edge).unwrap();
                prop_assert!(wire.is_string());
                prop_assert_eq!(by_map.inbound(ty, Some(wire)), Ok(edge));
            }

            let boolean = Transform { invert, ..Transform::default() };
            let wire = boolean.outbound(ValueType::Bool, &Value::Bool(flag)).unwrap();
            prop_assert_eq!(boolean.inbound(ValueType::Bool, Some(wire)), Ok(Value::Bool(flag)));
        }

        /// A coarser integer payload range survives the round trip payload → attribute → payload.
        #[test]
        fn coarse_scales_round_trip_from_the_wire(percent in 0_i64..=100, invert in any::<bool>()) {
            let transform = Transform {
                scale: Some(Scale { min: 0.0, max: 100.0 }),
                invert,
                ..Transform::default()
            };
            let attribute = transform.inbound(LEVEL, Some(json!(percent))).unwrap();
            prop_assert_eq!(transform.outbound(LEVEL, &attribute), Some(json!(percent)));
        }
    }
}
