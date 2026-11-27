//! The encoding of the values the bridge persists.
//!
//! A small, stable, self-describing binary encoding for scalar [`Value`]s, so that the bridge's
//! persisted state needs no serialisation framework. The first byte is a tag that names the kind;
//! integers follow in big-endian order in their own width, strings and octet strings as a 32-bit
//! big-endian length and the bytes. Lists and structures are not persisted by the bridge and
//! have no encoding.
//!
//! | Tag | Kind | Payload |
//! |---|---|---|
//! | `0x00` | null | none |
//! | `0x01` | bool | one byte, `0` or `1` |
//! | `0x02` .. `0x05` | u8, u16, u32, u64 | the integer, big-endian |
//! | `0x06` .. `0x09` | i8, i16, i32, i64 | the integer, big-endian two's complement |
//! | `0x0A`, `0x0B` | f32, f64 | IEEE 754 bits, big-endian |
//! | `0x0C` | UTF-8 string | u32 length, the bytes |
//! | `0x0D` | octet string | u32 length, the bytes |

use alloc::string::String;
use alloc::vec::Vec;

use matter_rs_engine::Value;

const TAG_NULL: u8 = 0x00;
const TAG_BOOL: u8 = 0x01;
const TAG_U8: u8 = 0x02;
const TAG_U16: u8 = 0x03;
const TAG_U32: u8 = 0x04;
const TAG_U64: u8 = 0x05;
const TAG_I8: u8 = 0x06;
const TAG_I16: u8 = 0x07;
const TAG_I32: u8 = 0x08;
const TAG_I64: u8 = 0x09;
const TAG_F32: u8 = 0x0A;
const TAG_F64: u8 = 0x0B;
const TAG_UTF8: u8 = 0x0C;
const TAG_OCTETS: u8 = 0x0D;

/// Encodes a scalar value; `None` for a list or a structure.
pub(crate) fn encode(value: &Value) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    match value {
        Value::Null => out.push(TAG_NULL),
        Value::Bool(v) => out.extend([TAG_BOOL, u8::from(*v)]),
        Value::U8(v) => out.extend([TAG_U8, *v]),
        Value::U16(v) => {
            out.push(TAG_U16);
            out.extend(v.to_be_bytes());
        }
        Value::U32(v) => {
            out.push(TAG_U32);
            out.extend(v.to_be_bytes());
        }
        Value::U64(v) => {
            out.push(TAG_U64);
            out.extend(v.to_be_bytes());
        }
        Value::I8(v) => {
            out.push(TAG_I8);
            out.extend(v.to_be_bytes());
        }
        Value::I16(v) => {
            out.push(TAG_I16);
            out.extend(v.to_be_bytes());
        }
        Value::I32(v) => {
            out.push(TAG_I32);
            out.extend(v.to_be_bytes());
        }
        Value::I64(v) => {
            out.push(TAG_I64);
            out.extend(v.to_be_bytes());
        }
        Value::F32(v) => {
            out.push(TAG_F32);
            out.extend(v.to_be_bytes());
        }
        Value::F64(v) => {
            out.push(TAG_F64);
            out.extend(v.to_be_bytes());
        }
        Value::Utf8(v) => {
            out.push(TAG_UTF8);
            push_bytes(&mut out, v.as_bytes());
        }
        Value::Octets(v) => {
            out.push(TAG_OCTETS);
            push_bytes(&mut out, v);
        }
        Value::List(_) | Value::Struct(_) => return None,
    }
    Some(out)
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    out.extend(length.to_be_bytes());
    out.extend(&bytes[..bytes.len().min(length as usize)]);
}

/// Decodes a value written by [`encode`]. The whole input must be one value.
pub(crate) fn decode(bytes: &[u8]) -> Result<Value, String> {
    let mut reader = Reader(bytes);
    let value = reader.value()?;
    if !reader.0.is_empty() {
        return Err(String::from("trailing bytes after the value"));
    }
    Ok(value)
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], String> {
        if self.0.len() < N {
            return Err(String::from("truncated value"));
        }
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        let mut array = [0; N];
        array.copy_from_slice(head);
        Ok(array)
    }

    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let length = u32::from_be_bytes(self.take()?) as usize;
        if self.0.len() < length {
            return Err(String::from("truncated string"));
        }
        let (head, rest) = self.0.split_at(length);
        self.0 = rest;
        Ok(head.to_vec())
    }

    fn value(&mut self) -> Result<Value, String> {
        let [tag] = self.take()?;
        let value = match tag {
            TAG_NULL => Value::Null,
            TAG_BOOL => match self.take()? {
                [0] => Value::Bool(false),
                [1] => Value::Bool(true),
                _ => return Err(String::from("invalid boolean")),
            },
            TAG_U8 => Value::U8(u8::from_be_bytes(self.take()?)),
            TAG_U16 => Value::U16(u16::from_be_bytes(self.take()?)),
            TAG_U32 => Value::U32(u32::from_be_bytes(self.take()?)),
            TAG_U64 => Value::U64(u64::from_be_bytes(self.take()?)),
            TAG_I8 => Value::I8(i8::from_be_bytes(self.take()?)),
            TAG_I16 => Value::I16(i16::from_be_bytes(self.take()?)),
            TAG_I32 => Value::I32(i32::from_be_bytes(self.take()?)),
            TAG_I64 => Value::I64(i64::from_be_bytes(self.take()?)),
            TAG_F32 => Value::F32(f32::from_be_bytes(self.take()?)),
            TAG_F64 => Value::F64(f64::from_be_bytes(self.take()?)),
            TAG_UTF8 => Value::Utf8(
                String::from_utf8(self.bytes()?).map_err(|_| String::from("invalid UTF-8"))?,
            ),
            TAG_OCTETS => Value::Octets(self.bytes()?),
            other => return Err(alloc::format!("unknown value tag 0x{other:02X}")),
        };
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::String;
    use alloc::vec;

    use matter_rs_engine::Value;
    use proptest::prelude::*;

    use super::{decode, encode};

    fn scalar() -> impl Strategy<Value = Value> {
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
            (-1.0e30_f32..1.0e30).prop_map(Value::F32),
            (-1.0e300_f64..1.0e300).prop_map(Value::F64),
            ".*".prop_map(Value::Utf8),
            proptest::collection::vec(any::<u8>(), 0..16).prop_map(Value::Octets),
        ]
    }

    proptest! {
        #[test]
        fn scalars_round_trip(value in scalar()) {
            let encoded = encode(&value).expect("scalars encode");
            prop_assert_eq!(decode(&encoded).unwrap(), value);
        }

        #[test]
        fn random_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..24)) {
            let _ = decode(&bytes);
        }
    }

    #[test]
    fn encoding_is_the_documented_one() {
        assert_eq!(encode(&Value::Null).unwrap(), vec![0x00]);
        assert_eq!(encode(&Value::Bool(true)).unwrap(), vec![0x01, 0x01]);
        assert_eq!(encode(&Value::U16(0x1234)).unwrap(), vec![0x03, 0x12, 0x34]);
        assert_eq!(encode(&Value::I8(-1)).unwrap(), vec![0x06, 0xFF]);
        assert_eq!(
            encode(&Value::Utf8(String::from("ab"))).unwrap(),
            vec![0x0C, 0, 0, 0, 2, b'a', b'b']
        );
        assert_eq!(encode(&Value::List(vec![])), None);
        assert_eq!(encode(&Value::Struct(vec![])), None);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        assert!(decode(&[]).unwrap_err().contains("truncated"));
        assert!(decode(&[0x03, 0x12]).unwrap_err().contains("truncated"));
        assert!(decode(&[0x01, 0x02]).unwrap_err().contains("boolean"));
        assert!(decode(&[0x00, 0x00]).unwrap_err().contains("trailing"));
        assert!(decode(&[0xFF]).unwrap_err().contains("tag"));
        assert!(
            decode(&[0x0C, 0, 0, 0, 9, b'a'])
                .unwrap_err()
                .contains("truncated")
        );
        assert!(
            decode(&[0x0C, 0, 0, 0, 1, 0xFF])
                .unwrap_err()
                .contains("UTF-8")
        );
    }
}
