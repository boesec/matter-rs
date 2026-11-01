//! The dynamic value that crosses the facade.
//!
//! Attribute values, command payloads and event payloads are carried as a [`Value`] tree that
//! maps one to one onto Matter TLV: every TLV element type has exactly one variant here. The
//! encoding itself belongs to the engine implementation; the facade only fixes the shape.
//!
//! Integer widths are the TLV widths. A Matter client encodes an integer with the smallest width
//! that holds it, so a write of `5` to a 16-bit attribute arrives as [`Value::U8`]. The
//! conversions back to Rust integers therefore accept any width of the same signedness whose
//! value fits, and refuse everything else with a [`ValueError`] that names the expected and the
//! found [`Kind`].

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A dynamic value, one variant per Matter TLV element type.
///
/// Enumerations and bitmaps are integers; a nullable attribute that is null is [`Value::Null`];
/// a structure is a list of `(field tag, value)` pairs in the order they are encoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// TLV null.
    Null,
    /// TLV boolean.
    Bool(bool),
    /// Unsigned integer, 1 octet.
    U8(u8),
    /// Unsigned integer, 2 octets.
    U16(u16),
    /// Unsigned integer, 4 octets.
    U32(u32),
    /// Unsigned integer, 8 octets.
    U64(u64),
    /// Signed integer, 1 octet.
    I8(i8),
    /// Signed integer, 2 octets.
    I16(i16),
    /// Signed integer, 4 octets.
    I32(i32),
    /// Signed integer, 8 octets.
    I64(i64),
    /// Single-precision float.
    F32(f32),
    /// Double-precision float.
    F64(f64),
    /// UTF-8 string.
    Utf8(String),
    /// Octet string.
    Octets(Vec<u8>),
    /// List (TLV array) of values.
    List(Vec<Value>),
    /// Structure: `(field tag, value)` pairs.
    Struct(Vec<(u64, Value)>),
}

impl Value {
    /// The kind of this value.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Null => Kind::Null,
            Self::Bool(_) => Kind::Bool,
            Self::U8(_) => Kind::U8,
            Self::U16(_) => Kind::U16,
            Self::U32(_) => Kind::U32,
            Self::U64(_) => Kind::U64,
            Self::I8(_) => Kind::I8,
            Self::I16(_) => Kind::I16,
            Self::I32(_) => Kind::I32,
            Self::I64(_) => Kind::I64,
            Self::F32(_) => Kind::F32,
            Self::F64(_) => Kind::F64,
            Self::Utf8(_) => Kind::Utf8,
            Self::Octets(_) => Kind::Octets,
            Self::List(_) => Kind::List,
            Self::Struct(_) => Kind::Struct,
        }
    }

    /// Whether the value is [`Value::Null`].
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// The value as an unsigned integer, whatever its width; `None` for other kinds.
    const fn as_unsigned(&self) -> Option<u64> {
        match self {
            Self::U8(v) => Some(*v as u64),
            Self::U16(v) => Some(*v as u64),
            Self::U32(v) => Some(*v as u64),
            Self::U64(v) => Some(*v),
            _ => None,
        }
    }

    /// The value as a signed integer, whatever its width; `None` for other kinds.
    const fn as_signed(&self) -> Option<i64> {
        match self {
            Self::I8(v) => Some(*v as i64),
            Self::I16(v) => Some(*v as i64),
            Self::I32(v) => Some(*v as i64),
            Self::I64(v) => Some(*v),
            _ => None,
        }
    }
}

/// The kind of a [`Value`], used to describe conversion failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// [`Value::Null`].
    Null,
    /// [`Value::Bool`].
    Bool,
    /// [`Value::U8`].
    U8,
    /// [`Value::U16`].
    U16,
    /// [`Value::U32`].
    U32,
    /// [`Value::U64`].
    U64,
    /// [`Value::I8`].
    I8,
    /// [`Value::I16`].
    I16,
    /// [`Value::I32`].
    I32,
    /// [`Value::I64`].
    I64,
    /// [`Value::F32`].
    F32,
    /// [`Value::F64`].
    F64,
    /// [`Value::Utf8`].
    Utf8,
    /// [`Value::Octets`].
    Octets,
    /// [`Value::List`].
    List,
    /// [`Value::Struct`].
    Struct,
}

impl Kind {
    /// The lower-case name of the kind, as it appears in error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Utf8 => "utf8",
            Self::Octets => "octets",
            Self::List => "list",
            Self::Struct => "struct",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A [`Value`] could not be converted to the requested Rust type.
///
/// Both variants name the kind that was expected and the kind that was found, so that a
/// mismatch can be reported without looking at the value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ValueError {
    /// The value has a different kind than the target type.
    #[error("expected {expected}, found {found}")]
    Kind {
        /// The kind the target type converts from.
        expected: Kind,
        /// The kind of the value that was offered.
        found: Kind,
    },
    /// The value has the right signedness but does not fit the target type.
    #[error("expected {expected}, found a {found} outside its range")]
    OutOfRange {
        /// The kind of the target type.
        expected: Kind,
        /// The kind of the value that was offered.
        found: Kind,
    },
}

macro_rules! from_primitive {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$ty> for Value {
                fn from(v: $ty) -> Self {
                    Self::$variant(v)
                }
            }
        )*
    };
}

from_primitive! {
    bool => Bool,
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
    i8 => I8,
    i16 => I16,
    i32 => I32,
    i64 => I64,
    f32 => F32,
    f64 => F64,
    String => Utf8,
    Vec<u8> => Octets,
    Vec<Value> => List,
    Vec<(u64, Value)> => Struct,
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::Utf8(String::from(v))
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    /// `None` becomes [`Value::Null`], `Some(v)` converts `v`.
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

macro_rules! try_from_unsigned {
    ($($ty:ty => $kind:ident),* $(,)?) => {
        $(
            impl TryFrom<Value> for $ty {
                type Error = ValueError;

                /// Accepts any unsigned width whose value fits.
                fn try_from(value: Value) -> Result<Self, Self::Error> {
                    let found = value.kind();
                    let raw = value.as_unsigned().ok_or(ValueError::Kind {
                        expected: Kind::$kind,
                        found,
                    })?;
                    Self::try_from(raw).map_err(|_| ValueError::OutOfRange {
                        expected: Kind::$kind,
                        found,
                    })
                }
            }
        )*
    };
}

try_from_unsigned! {
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
}

macro_rules! try_from_signed {
    ($($ty:ty => $kind:ident),* $(,)?) => {
        $(
            impl TryFrom<Value> for $ty {
                type Error = ValueError;

                /// Accepts any signed width whose value fits.
                fn try_from(value: Value) -> Result<Self, Self::Error> {
                    let found = value.kind();
                    let raw = value.as_signed().ok_or(ValueError::Kind {
                        expected: Kind::$kind,
                        found,
                    })?;
                    Self::try_from(raw).map_err(|_| ValueError::OutOfRange {
                        expected: Kind::$kind,
                        found,
                    })
                }
            }
        )*
    };
}

try_from_signed! {
    i8 => I8,
    i16 => I16,
    i32 => I32,
    i64 => I64,
}

macro_rules! try_from_exact {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(
            impl TryFrom<Value> for $ty {
                type Error = ValueError;

                fn try_from(value: Value) -> Result<Self, Self::Error> {
                    match value {
                        Value::$variant(v) => Ok(v),
                        other => Err(ValueError::Kind {
                            expected: Kind::$variant,
                            found: other.kind(),
                        }),
                    }
                }
            }
        )*
    };
}

try_from_exact! {
    bool => Bool,
    f32 => F32,
    String => Utf8,
    Vec<u8> => Octets,
    Vec<Value> => List,
    Vec<(u64, Value)> => Struct,
}

impl TryFrom<Value> for f64 {
    type Error = ValueError;

    /// Accepts [`Value::F64`] and, because the widening is lossless, [`Value::F32`].
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        match value {
            Value::F64(v) => Ok(v),
            Value::F32(v) => Ok(Self::from(v)),
            other => Err(ValueError::Kind {
                expected: Kind::F64,
                found: other.kind(),
            }),
        }
    }
}

impl<T> TryFrom<Value> for Option<T>
where
    T: TryFrom<Value, Error = ValueError>,
{
    type Error = ValueError;

    /// [`Value::Null`] becomes `None`; anything else converts to `T`.
    fn try_from(value: Value) -> Result<Self, Self::Error> {
        if value.is_null() {
            Ok(None)
        } else {
            T::try_from(value).map(Some)
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use proptest::prelude::*;

    use super::{Kind, Value, ValueError};

    /// Every `From` has a matching `TryFrom` that gives the original back.
    fn round_trip<T>(original: T)
    where
        T: Into<Value> + TryFrom<Value, Error = ValueError> + PartialEq + core::fmt::Debug + Clone,
    {
        let value: Value = original.clone().into();
        let back = T::try_from(value).expect("conversion back succeeds");
        assert_eq!(back, original);
    }

    fn leaf() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<u8>().prop_map(Value::U8),
            any::<u16>().prop_map(Value::U16),
            any::<u32>().prop_map(Value::U32),
            any::<u64>().prop_map(Value::U64),
            any::<i8>().prop_map(Value::I8),
            any::<i16>().prop_map(Value::I16),
            any::<i32>().prop_map(Value::I32),
            any::<i64>().prop_map(Value::I64),
            // Finite floats only: NaN is never equal to itself, so it cannot round trip through
            // an equality check.
            (-1.0e30_f32..1.0e30).prop_map(Value::F32),
            (-1.0e300_f64..1.0e300).prop_map(Value::F64),
            ".*".prop_map(Value::Utf8),
            proptest::collection::vec(any::<u8>(), 0..16).prop_map(Value::Octets),
        ]
    }

    fn value() -> impl Strategy<Value = Value> {
        leaf().prop_recursive(3, 32, 4, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::List),
                proptest::collection::vec((any::<u64>(), inner), 0..4).prop_map(Value::Struct),
            ]
        })
    }

    proptest! {
        #[test]
        fn value_round_trip(
            b in any::<bool>(),
            u8v in any::<u8>(), u16v in any::<u16>(), u32v in any::<u32>(), u64v in any::<u64>(),
            i8v in any::<i8>(), i16v in any::<i16>(), i32v in any::<i32>(), i64v in any::<i64>(),
            f32v in -1.0e30_f32..1.0e30, f64v in -1.0e300_f64..1.0e300,
            s in ".*",
            octets in proptest::collection::vec(any::<u8>(), 0..32),
            list in proptest::collection::vec(value(), 0..4),
            fields in proptest::collection::vec((any::<u64>(), value()), 0..4),
            option in proptest::option::of(any::<u32>()),
        ) {
            round_trip(b);
            round_trip(u8v);
            round_trip(u16v);
            round_trip(u32v);
            round_trip(u64v);
            round_trip(i8v);
            round_trip(i16v);
            round_trip(i32v);
            round_trip(i64v);
            round_trip(f32v);
            round_trip(f64v);
            round_trip(s.clone());
            round_trip(octets);
            round_trip(list);
            round_trip(fields);
            round_trip(option);
            // `&str` converts in but comes back as `String`.
            prop_assert_eq!(String::try_from(Value::from(s.as_str())).unwrap(), s);
        }

        #[test]
        fn narrow_unsigned_widens_without_loss(v in any::<u8>()) {
            prop_assert_eq!(u16::try_from(Value::U8(v)).unwrap(), u16::from(v));
            prop_assert_eq!(u64::try_from(Value::U8(v)).unwrap(), u64::from(v));
        }

        #[test]
        fn narrow_signed_widens_without_loss(v in any::<i8>()) {
            prop_assert_eq!(i16::try_from(Value::I8(v)).unwrap(), i16::from(v));
            prop_assert_eq!(i64::try_from(Value::I8(v)).unwrap(), i64::from(v));
        }

        #[test]
        fn wide_value_that_fits_narrows(v in any::<u8>(), s in any::<i8>()) {
            prop_assert_eq!(u8::try_from(Value::U64(u64::from(v))).unwrap(), v);
            prop_assert_eq!(i8::try_from(Value::I32(i32::from(s))).unwrap(), s);
        }
    }

    #[test]
    fn value_mismatch_names_kinds() {
        let error = u8::try_from(Value::Utf8("on".to_string())).unwrap_err();
        assert_eq!(
            error,
            ValueError::Kind {
                expected: Kind::U8,
                found: Kind::Utf8
            }
        );
        assert_eq!(error.to_string(), "expected u8, found utf8");

        let error = bool::try_from(Value::U8(1)).unwrap_err();
        assert_eq!(error.to_string(), "expected bool, found u8");

        let error = String::try_from(Value::Null).unwrap_err();
        assert_eq!(error.to_string(), "expected utf8, found null");

        let error = Vec::<Value>::try_from(Value::Struct(vec![])).unwrap_err();
        assert_eq!(error.to_string(), "expected list, found struct");

        let error = f32::try_from(Value::F64(1.0)).unwrap_err();
        assert_eq!(error.to_string(), "expected f32, found f64");
    }

    #[test]
    fn signedness_is_not_crossed() {
        let error = u8::try_from(Value::I8(1)).unwrap_err();
        assert_eq!(error.to_string(), "expected u8, found i8");
        let error = i8::try_from(Value::U8(1)).unwrap_err();
        assert_eq!(error.to_string(), "expected i8, found u8");
    }

    #[test]
    fn out_of_range_names_kinds() {
        let error = u8::try_from(Value::U16(256)).unwrap_err();
        assert_eq!(
            error,
            ValueError::OutOfRange {
                expected: Kind::U8,
                found: Kind::U16
            }
        );
        assert_eq!(
            error.to_string(),
            "expected u8, found a u16 outside its range"
        );
        let error = i8::try_from(Value::I16(-129)).unwrap_err();
        assert_eq!(
            error.to_string(),
            "expected i8, found a i16 outside its range"
        );
    }

    #[test]
    fn f32_widens_to_f64() {
        assert_eq!(f64::try_from(Value::F32(0.5)).unwrap(), 0.5);
    }

    #[test]
    fn option_maps_to_null() {
        assert_eq!(Value::from(None::<u8>), Value::Null);
        assert_eq!(Value::from(Some(3_u8)), Value::U8(3));
        assert_eq!(Option::<u8>::try_from(Value::Null).unwrap(), None);
        assert_eq!(Option::<u8>::try_from(Value::U8(3)).unwrap(), Some(3));
        let error = Option::<u8>::try_from(Value::Bool(true)).unwrap_err();
        assert_eq!(error.to_string(), "expected u8, found bool");
    }

    #[test]
    fn kind_reports_the_variant() {
        assert_eq!(Value::Null.kind(), Kind::Null);
        assert_eq!(Value::List(vec![]).kind(), Kind::List);
        assert_eq!(Kind::Octets.name(), "octets");
        assert!(Value::Null.is_null());
        assert!(!Value::Bool(false).is_null());
    }
}
