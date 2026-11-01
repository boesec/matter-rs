//! The facade's dynamic [`Value`] on rs-matter's TLV: encoding a value for a read or a command
//! response, decoding the data of a write or a command.
//!
//! The mapping is the one the facade fixes: every TLV element type is one `Value` variant.
//! Structure field tags are carried as `u64`: a context tag (0..=255) maps to the number
//! itself, a fully qualified 64-bit tag to `vendor_id << 48 | profile << 32 | tag`, the profile
//! tags of 16 and 32 bits to their number. Encoding reverses that: a tag that fits a byte is a
//! context tag, anything else a fully qualified 64-bit tag.

use matter_rs_engine::Value;
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::{TLV, TLVElement, TLVTag, TLVValue, TLVWrite, ToTLV};

/// Encodes a facade value as TLV, for [`rs_matter::dm::Reply::set`] and event writers.
pub(crate) struct ValueTlv<'a>(pub(crate) &'a Value);

impl ToTLV for ValueTlv<'_> {
    fn to_tlv<W: TLVWrite>(&self, tag: &TLVTag, mut tw: W) -> Result<(), Error> {
        write_value(self.0, tag, &mut tw)
    }

    fn tlv_iter(&self, tag: TLVTag) -> impl Iterator<Item = Result<TLV<'_>, Error>> {
        let mut elements = Vec::new();
        flatten(self.0, tag, &mut elements);
        elements.into_iter().map(Ok)
    }
}

/// Writes `value` with `tag` into `tw`, recursing into containers.
pub(crate) fn write_value<W: TLVWrite>(
    value: &Value,
    tag: &TLVTag,
    tw: &mut W,
) -> Result<(), Error> {
    match value {
        Value::Null => tw.null(tag),
        Value::Bool(v) => tw.bool(tag, *v),
        Value::U8(v) => tw.u8(tag, *v),
        Value::U16(v) => tw.u16(tag, *v),
        Value::U32(v) => tw.u32(tag, *v),
        Value::U64(v) => tw.u64(tag, *v),
        Value::I8(v) => tw.i8(tag, *v),
        Value::I16(v) => tw.i16(tag, *v),
        Value::I32(v) => tw.i32(tag, *v),
        Value::I64(v) => tw.i64(tag, *v),
        Value::F32(v) => tw.f32(tag, *v),
        Value::F64(v) => tw.f64(tag, *v),
        Value::Utf8(v) => tw.utf8(tag, v),
        Value::Octets(v) => tw.str(tag, v),
        Value::List(items) => {
            tw.start_array(tag)?;
            for item in items {
                write_value(item, &TLVTag::Anonymous, tw)?;
            }
            tw.end_container()
        }
        Value::Struct(fields) => {
            tw.start_struct(tag)?;
            for (field_tag, field) in fields {
                write_value(field, &tag_from_raw(*field_tag), tw)?;
            }
            tw.end_container()
        }
    }
}

/// Writes the fields of an event payload: a struct becomes the fields of the event data
/// structure, null an empty structure, anything else a single anonymous-free field with tag 0.
pub(crate) fn write_event_fields<W: TLVWrite>(
    payload: &Value,
    data_tag: &TLVTag,
    tw: &mut W,
) -> Result<(), Error> {
    tw.start_struct(data_tag)?;
    match payload {
        Value::Struct(fields) => {
            for (field_tag, field) in fields {
                write_value(field, &tag_from_raw(*field_tag), tw)?;
            }
        }
        Value::Null => {}
        other => write_value(other, &TLVTag::Context(0), tw)?,
    }
    tw.end_container()
}

fn flatten<'a>(value: &'a Value, tag: TLVTag, out: &mut Vec<TLV<'a>>) {
    match value {
        Value::Null => out.push(TLV::null(tag)),
        Value::Bool(v) => out.push(TLV::bool(tag, *v)),
        Value::U8(v) => out.push(TLV::u8(tag, *v)),
        Value::U16(v) => out.push(TLV::u16(tag, *v)),
        Value::U32(v) => out.push(TLV::u32(tag, *v)),
        Value::U64(v) => out.push(TLV::u64(tag, *v)),
        Value::I8(v) => out.push(TLV::i8(tag, *v)),
        Value::I16(v) => out.push(TLV::i16(tag, *v)),
        Value::I32(v) => out.push(TLV::i32(tag, *v)),
        Value::I64(v) => out.push(TLV::i64(tag, *v)),
        Value::F32(v) => out.push(TLV::f32(tag, *v)),
        Value::F64(v) => out.push(TLV::f64(tag, *v)),
        Value::Utf8(v) => out.push(TLV::utf8(tag, v)),
        Value::Octets(v) => out.push(TLV::str(tag, v)),
        Value::List(items) => {
            out.push(TLV::array(tag));
            for item in items {
                flatten(item, TLVTag::Anonymous, out);
            }
            out.push(TLV::end_container());
        }
        Value::Struct(fields) => {
            out.push(TLV::structure(tag));
            for (field_tag, field) in fields {
                flatten(field, tag_from_raw(*field_tag), out);
            }
            out.push(TLV::end_container());
        }
    }
}

/// The TLV tag for a facade field tag.
pub(crate) fn tag_from_raw(raw: u64) -> TLVTag {
    match u8::try_from(raw) {
        Ok(context) => TLVTag::Context(context),
        Err(_) => TLVTag::FullQual64 {
            vendor_id: (raw >> 48) as u16,
            profile: ((raw >> 32) & 0xFFFF) as u16,
            tag: (raw & 0xFFFF_FFFF) as u32,
        },
    }
}

/// The facade field tag for a TLV tag; an anonymous tag inside a structure becomes 0.
pub(crate) fn raw_from_tag(tag: &TLVTag) -> u64 {
    match tag {
        TLVTag::Anonymous => 0,
        TLVTag::Context(c) => u64::from(*c),
        TLVTag::CommonPrf16(t) | TLVTag::ImplPrf16(t) => u64::from(*t),
        TLVTag::CommonPrf32(t) | TLVTag::ImplPrf32(t) => u64::from(*t),
        TLVTag::FullQual48 {
            vendor_id,
            profile,
            tag,
        } => (u64::from(*vendor_id) << 48) | (u64::from(*profile) << 32) | u64::from(*tag),
        TLVTag::FullQual64 {
            vendor_id,
            profile,
            tag,
        } => (u64::from(*vendor_id) << 48) | (u64::from(*profile) << 32) | u64::from(*tag),
    }
}

/// Decodes a TLV element into a facade value.
///
/// # Errors
///
/// rs-matter's error when the element is malformed.
pub(crate) fn value_from_tlv(element: &TLVElement<'_>) -> Result<Value, Error> {
    Ok(match element.value()? {
        TLVValue::S8(v) => Value::I8(v),
        TLVValue::S16(v) => Value::I16(v),
        TLVValue::S32(v) => Value::I32(v),
        TLVValue::S64(v) => Value::I64(v),
        TLVValue::U8(v) => Value::U8(v),
        TLVValue::U16(v) => Value::U16(v),
        TLVValue::U32(v) => Value::U32(v),
        TLVValue::U64(v) => Value::U64(v),
        TLVValue::False => Value::Bool(false),
        TLVValue::True => Value::Bool(true),
        TLVValue::F32(v) => Value::F32(v),
        TLVValue::F64(v) => Value::F64(v),
        TLVValue::Utf8l(s) | TLVValue::Utf16l(s) | TLVValue::Utf32l(s) | TLVValue::Utf64l(s) => {
            Value::Utf8(s.to_owned())
        }
        TLVValue::Str8l(b) | TLVValue::Str16l(b) | TLVValue::Str32l(b) | TLVValue::Str64l(b) => {
            Value::Octets(b.to_vec())
        }
        TLVValue::Null => Value::Null,
        TLVValue::Struct => {
            let mut fields = Vec::new();
            for item in element.container()?.iter() {
                let item = item?;
                fields.push((raw_from_tag(&item.tag()?), value_from_tlv(&item)?));
            }
            Value::Struct(fields)
        }
        TLVValue::Array | TLVValue::List => {
            let mut items = Vec::new();
            for item in element.container()?.iter() {
                items.push(value_from_tlv(&item?)?);
            }
            Value::List(items)
        }
        TLVValue::EndCnt => return Err(ErrorCode::InvalidData.into()),
    })
}

/// Encodes `value` into a fresh buffer; for tests and for the array read protocol.
#[cfg(test)]
pub(crate) fn encode(value: &Value) -> Vec<u8> {
    use rs_matter::utils::storage::WriteBuf;

    let mut buffer = vec![0_u8; 4096];
    let mut wb = WriteBuf::new(&mut buffer);
    write_value(value, &TLVTag::Anonymous, &mut wb).expect("value fits");
    wb.as_slice().to_vec()
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::Value;
    use rs_matter::tlv::{TLVElement, TLVTag};

    use super::{encode, raw_from_tag, tag_from_raw, value_from_tlv};

    fn round_trip(value: Value) {
        let bytes = encode(&value);
        let decoded = value_from_tlv(&TLVElement::new(&bytes)).expect("decodes");
        assert_eq!(decoded, value);
    }

    #[test]
    fn scalars_round_trip() {
        round_trip(Value::Null);
        round_trip(Value::Bool(true));
        round_trip(Value::Bool(false));
        round_trip(Value::U8(200));
        round_trip(Value::U16(60_000));
        round_trip(Value::U32(4_000_000_000));
        round_trip(Value::U64(u64::MAX));
        round_trip(Value::I8(-100));
        round_trip(Value::I16(-30_000));
        round_trip(Value::I32(-2_000_000_000));
        round_trip(Value::I64(i64::MIN));
        round_trip(Value::F32(1.5));
        round_trip(Value::F64(-2.25));
        round_trip(Value::Utf8("Kitchen".into()));
        round_trip(Value::Octets(vec![1, 2, 3]));
    }

    #[test]
    fn containers_round_trip() {
        round_trip(Value::List(vec![Value::U8(1), Value::Utf8("a".into())]));
        round_trip(Value::List(vec![]));
        round_trip(Value::Struct(vec![
            (0, Value::Bool(true)),
            (
                1,
                Value::List(vec![Value::Struct(vec![(5, Value::U16(700))])]),
            ),
            (0x0001_0002_0000_0003, Value::Null),
        ]));
    }

    #[test]
    fn integers_come_back_in_the_smallest_width() {
        // Matter encodes an integer in the smallest width that holds it; the facade's
        // conversions accept any width of the same signedness.
        let bytes = encode(&Value::U16(7));
        assert_eq!(
            value_from_tlv(&TLVElement::new(&bytes)).unwrap(),
            Value::U8(7)
        );
        let bytes = encode(&Value::I64(-3));
        assert_eq!(
            value_from_tlv(&TLVElement::new(&bytes)).unwrap(),
            Value::I8(-3)
        );
    }

    #[test]
    fn tags_map_both_ways() {
        assert_eq!(tag_from_raw(3), TLVTag::Context(3));
        assert_eq!(
            tag_from_raw(0x1234_5678_0000_0009),
            TLVTag::FullQual64 {
                vendor_id: 0x1234,
                profile: 0x5678,
                tag: 9
            }
        );
        assert_eq!(raw_from_tag(&TLVTag::Anonymous), 0);
        assert_eq!(raw_from_tag(&TLVTag::CommonPrf16(300)), 300);
        assert_eq!(
            raw_from_tag(&TLVTag::FullQual48 {
                vendor_id: 1,
                profile: 2,
                tag: 3
            }),
            0x0001_0002_0000_0003
        );
    }
}
