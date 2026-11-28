//! The attributes a device mapping can name, with their default item names and types.
//!
//! A mapping addresses an attribute by its kebab-case name (`on-off`, `level`); the default item
//! name on the broker is the same word in `snake_case` (`on_off`, `level`). The types and ranges
//! are those of the bridge's device kinds; the table says which kinds each attribute belongs to.
//! Attributes a source rarely publishes (the physical colour temperature bounds) have no default
//! item and must be given a topic or a fixed `value`.

use matter_rs_bridge::{Attribute, DeviceKind};

use crate::transform::{IntKind, ValueType};

/// One attribute a mapping can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappableAttribute {
    /// The kebab-case name used in `mapping.items`.
    pub name: &'static str,
    /// The default item name on the broker, in `snake_case`.
    pub item: &'static str,
    /// The bridge attribute.
    pub attribute: Attribute,
    /// The type and range.
    pub value_type: ValueType,
    /// The kinds that serve the attribute.
    pub kinds: &'static [DeviceKind],
    /// Whether the attribute is mapped by default (to `<source>/status/<device>/<item>` and the
    /// matching `set` topic). Without it, the item exists only when the mapping names it.
    pub by_default: bool,
}

const LIGHTS: &[DeviceKind] = &[
    DeviceKind::OnOffLight,
    DeviceKind::DimmableLight,
    DeviceKind::ColorTemperatureLight,
    DeviceKind::ExtendedColorLight,
    DeviceKind::OnOffPlugInUnit,
];
const DIMMABLE: &[DeviceKind] = &[
    DeviceKind::DimmableLight,
    DeviceKind::ColorTemperatureLight,
    DeviceKind::ExtendedColorLight,
];
const COLOR_TEMPERATURE: &[DeviceKind] = &[
    DeviceKind::ColorTemperatureLight,
    DeviceKind::ExtendedColorLight,
];
const COLOR: &[DeviceKind] = &[DeviceKind::ExtendedColorLight];

const fn integer(kind: IntKind, min: i64, max: i64, nullable: bool) -> ValueType {
    ValueType::Integer {
        kind,
        min,
        max,
        nullable,
    }
}

/// Mireds as the Color Control cluster bounds them.
const MIREDS: ValueType = integer(IntKind::U16, 0, 0xFEFF, false);

/// Every mappable attribute, in the order of the contract's table.
pub const ALL: &[MappableAttribute] = &[
    MappableAttribute {
        name: "on-off",
        item: "on_off",
        attribute: Attribute::OnOff,
        value_type: ValueType::Bool,
        kinds: LIGHTS,
        by_default: true,
    },
    MappableAttribute {
        name: "level",
        item: "level",
        attribute: Attribute::CurrentLevel,
        value_type: integer(IntKind::U8, 1, 254, true),
        kinds: DIMMABLE,
        by_default: true,
    },
    MappableAttribute {
        name: "color-temperature",
        item: "color_temperature",
        attribute: Attribute::ColorTemperatureMireds,
        value_type: MIREDS,
        kinds: COLOR_TEMPERATURE,
        by_default: true,
    },
    MappableAttribute {
        name: "color-temperature-min",
        item: "color_temperature_min",
        attribute: Attribute::ColorTempPhysicalMinMireds,
        value_type: MIREDS,
        kinds: COLOR_TEMPERATURE,
        by_default: false,
    },
    MappableAttribute {
        name: "color-temperature-max",
        item: "color_temperature_max",
        attribute: Attribute::ColorTempPhysicalMaxMireds,
        value_type: MIREDS,
        kinds: COLOR_TEMPERATURE,
        by_default: false,
    },
    MappableAttribute {
        name: "hue",
        item: "hue",
        attribute: Attribute::CurrentHue,
        value_type: integer(IntKind::U8, 0, 254, false),
        kinds: COLOR,
        by_default: true,
    },
    MappableAttribute {
        name: "saturation",
        item: "saturation",
        attribute: Attribute::CurrentSaturation,
        value_type: integer(IntKind::U8, 0, 254, false),
        kinds: COLOR,
        by_default: true,
    },
    MappableAttribute {
        name: "color-x",
        item: "color_x",
        attribute: Attribute::CurrentX,
        value_type: integer(IntKind::U16, 0, 0xFEFF, false),
        kinds: COLOR,
        by_default: true,
    },
    MappableAttribute {
        name: "color-y",
        item: "color_y",
        attribute: Attribute::CurrentY,
        value_type: integer(IntKind::U16, 0, 0xFEFF, false),
        kinds: COLOR,
        by_default: true,
    },
    MappableAttribute {
        name: "state",
        item: "state",
        attribute: Attribute::StateValue,
        value_type: ValueType::Bool,
        kinds: &[DeviceKind::ContactSensor],
        by_default: true,
    },
    MappableAttribute {
        name: "temperature",
        item: "temperature",
        attribute: Attribute::Temperature,
        value_type: integer(IntKind::I16, -27315, 32767, true),
        kinds: &[DeviceKind::TemperatureSensor],
        by_default: true,
    },
    MappableAttribute {
        name: "humidity",
        item: "humidity",
        attribute: Attribute::Humidity,
        value_type: integer(IntKind::U16, 0, 10000, true),
        kinds: &[DeviceKind::HumiditySensor],
        by_default: true,
    },
    MappableAttribute {
        name: "pressure",
        item: "pressure",
        attribute: Attribute::Pressure,
        value_type: integer(IntKind::I16, i16::MIN as i64, i16::MAX as i64, true),
        kinds: &[DeviceKind::PressureSensor],
        by_default: true,
    },
    MappableAttribute {
        name: "illuminance",
        item: "illuminance",
        attribute: Attribute::Illuminance,
        value_type: integer(IntKind::U16, 0, 0xFFFE, true),
        kinds: &[DeviceKind::LightSensor],
        by_default: true,
    },
    MappableAttribute {
        name: "occupancy",
        item: "occupancy",
        attribute: Attribute::Occupancy,
        value_type: integer(IntKind::U8, 0, 1, false),
        kinds: &[DeviceKind::OccupancySensor],
        by_default: true,
    },
];

/// The pseudo item for the device's label: a controller's rename is published to its `set`
/// topic when the mapping lists it. It is no attribute and has no status.
pub const NAME_ITEM: &str = "name";

/// The attribute with this kebab-case name.
#[must_use]
pub fn by_name(name: &str) -> Option<&'static MappableAttribute> {
    ALL.iter().find(|attribute| attribute.name == name)
}

/// The attributes of a kind, in table order.
pub fn of_kind(kind: DeviceKind) -> impl Iterator<Item = &'static MappableAttribute> {
    ALL.iter()
        .filter(move |attribute| attribute.kinds.contains(&kind))
}

/// The placeholder an attribute carries from the moment the device is added until its first
/// value arrives; the device is unreachable meanwhile. Null where the attribute allows it, the
/// cluster's own "off" value otherwise.
#[must_use]
pub fn placeholder(attribute: &MappableAttribute) -> matter_rs_engine::Value {
    match attribute.value_type {
        ValueType::Bool => matter_rs_engine::Value::Bool(false),
        ValueType::Integer { nullable: true, .. } => matter_rs_engine::Value::Null,
        ValueType::Integer { min, .. } => attribute.value_type.integer(min as f64),
    }
}

#[cfg(test)]
mod tests {
    use matter_rs_bridge::{Attribute, DeviceKind};
    use matter_rs_engine::Value;

    use super::{ALL, by_name, of_kind, placeholder};

    #[test]
    fn names_and_items_are_unique_and_well_formed() {
        for (i, attribute) in ALL.iter().enumerate() {
            assert!(
                !ALL[..i].iter().any(|other| other.name == attribute.name
                    || other.item == attribute.item
                    || other.attribute == attribute.attribute),
                "{} is listed twice",
                attribute.name
            );
            assert_eq!(attribute.item, attribute.name.replace('-', "_"));
            assert!(
                attribute
                    .name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-')
            );
            assert!(!attribute.kinds.is_empty());
        }
        assert_eq!(
            by_name("on-off").map(|a| a.attribute),
            Some(Attribute::OnOff)
        );
        assert!(by_name("brightness").is_none());
    }

    #[test]
    fn every_required_attribute_of_every_kind_is_mappable() {
        for kind in DeviceKind::ALL {
            if kind == DeviceKind::GenericSwitch {
                continue;
            }
            let mappable: Vec<Attribute> = of_kind(kind).map(|a| a.attribute).collect();
            for required in kind.required_attributes() {
                assert!(
                    mappable.contains(&required),
                    "{kind:?} requires {required:?}, which the table does not map"
                );
            }
        }
        assert_eq!(of_kind(DeviceKind::GenericSwitch).count(), 0);
        assert_eq!(of_kind(DeviceKind::OnOffLight).count(), 1);
        assert_eq!(of_kind(DeviceKind::ExtendedColorLight).count(), 9);
    }

    #[test]
    fn placeholders_are_null_or_off() {
        assert_eq!(placeholder(by_name("on-off").unwrap()), Value::Bool(false));
        assert_eq!(placeholder(by_name("level").unwrap()), Value::Null);
        assert_eq!(placeholder(by_name("occupancy").unwrap()), Value::U8(0));
        assert_eq!(
            placeholder(by_name("color-temperature").unwrap()),
            Value::U16(0)
        );
    }
}
