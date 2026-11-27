//! The device model adapters work with: keys, specifications, typed attributes and events.
//!
//! Adapters describe a device as a [`DeviceSpec`] (a stable key, a name and one part per device
//! kind), feed values through typed [`Attribute`]s and report switch input as [`DeviceEvent`]s.
//! None of this names a cluster or an attribute id; the [`Attribute::Raw`] and
//! [`DeviceEvent::Raw`] escapes exist for the clusters a part lists on top of its kind.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use matter_rs_engine::{AttributeId, ClusterDescription, ClusterId, EventId, Value, VendorId};

use crate::error::Error;
use crate::ids::{
    boolean_state, color_control, identify, illuminance_measurement, level_control,
    occupancy_sensing, on_off, pressure_measurement, relative_humidity_measurement, switch,
    temperature_measurement,
};
use crate::kinds::DeviceKind;

/// The longest device key, in bytes.
///
/// The key is part of every storage key of the device (`bridge/attributes/<key>/...`), and a
/// storage back-end may limit the length of a key; 64 bytes leave room for the rest.
pub const MAX_DEVICE_KEY_LENGTH: usize = 64;

/// The stable identity of a bridged device: the id the configuration gives it.
///
/// The key decides which endpoint numbers a device gets back after a restart and under which
/// storage keys its persisted attributes live, so it must not change for the lifetime of the
/// device. It is a non-empty string of at most [`MAX_DEVICE_KEY_LENGTH`] bytes without control
/// characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceKey(String);

impl DeviceKey {
    /// Validates and wraps a key.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidKey`] when the key is empty, longer than [`MAX_DEVICE_KEY_LENGTH`] bytes
    /// or contains a control character.
    pub fn new(key: impl Into<String>) -> Result<Self, Error> {
        let key = key.into();
        if key.is_empty() {
            return Err(Error::InvalidKey {
                reason: "the key is empty",
            });
        }
        if key.len() > MAX_DEVICE_KEY_LENGTH {
            return Err(Error::InvalidKey {
                reason: "the key is longer than 64 bytes",
            });
        }
        if key.chars().any(char::is_control) {
            return Err(Error::InvalidKey {
                reason: "the key contains a control character",
            });
        }
        Ok(Self(key))
    }

    /// The key as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for DeviceKey {
    type Error = Error;

    fn try_from(key: &str) -> Result<Self, Self::Error> {
        Self::new(key)
    }
}

impl TryFrom<String> for DeviceKey {
    type Error = Error;

    fn try_from(key: String) -> Result<Self, Self::Error> {
        Self::new(key)
    }
}

/// The index of a part within a device, in the order of [`DeviceSpec::parts`].
pub type PartIndex = u16;

/// The most parts a device may have.
pub const MAX_PARTS: usize = 64;

/// What an adapter adds to the bridge: a device with one or more parts.
///
/// A device with one part is a plain bridged device: one endpoint that carries the part's device
/// type and the Bridged Node device type. A device with several parts is composed: a Bridged Node
/// endpoint as parent and one child endpoint per part, with semantic tags on children that share
/// a kind (see [`PartSpec::tags`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSpec {
    /// The stable key of the device.
    pub key: DeviceKey,
    /// The user-visible name, served as the `NodeLabel` of the device until a controller renames
    /// it.
    pub name: String,
    /// The parts of the device, at least one and at most [`MAX_PARTS`].
    pub parts: Vec<PartSpec>,
}

impl DeviceSpec {
    /// A device with one part of the given kind.
    #[must_use]
    pub fn single(key: DeviceKey, name: impl Into<String>, kind: DeviceKind) -> Self {
        Self {
            key,
            name: name.into(),
            parts: alloc::vec![PartSpec::new(kind)],
        }
    }

    /// Whether the device is composed of several parts.
    #[must_use]
    pub fn is_composed(&self) -> bool {
        self.parts.len() > 1
    }
}

/// One part of a device: a device kind with an optional label and semantic tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartSpec {
    /// The device kind of the part.
    pub kind: DeviceKind,
    /// A label for the part of a composed device, served as the label of its semantic tag.
    pub label: Option<String>,
    /// Semantic tags of the part of a composed device. When empty, the bridge adds a `Common
    /// Number` tag (`One`, `Two`, ...) to every part that shares its kind with another part, and
    /// to every part with a label.
    pub tags: Vec<SemanticTag>,
    /// Clusters the part serves on top of those of its kind, for example a vendor cluster.
    ///
    /// Commands and writes on these clusters reach the adapter unchanged as
    /// [`DeviceCommand::Raw`](crate::DeviceCommand::Raw) and
    /// [`DeviceCommand::Write`](crate::DeviceCommand::Write); their values are set through
    /// [`Attribute::Raw`]. Every listed attribute needs an initial value; the clusters of the kind,
    /// the Descriptor and the Bridged Device Basic Information clusters cannot be listed here.
    pub extra_clusters: Vec<ClusterDescription>,
}

impl PartSpec {
    /// A part of the given kind without label, tags or extra clusters.
    #[must_use]
    pub fn new(kind: DeviceKind) -> Self {
        Self {
            kind,
            label: None,
            tags: Vec::new(),
            extra_clusters: Vec::new(),
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Adds a semantic tag.
    #[must_use]
    pub fn with_tag(mut self, tag: SemanticTag) -> Self {
        self.tags.push(tag);
        self
    }

    /// Adds an extra cluster.
    #[must_use]
    pub fn with_extra_cluster(mut self, cluster: ClusterDescription) -> Self {
        self.extra_clusters.push(cluster);
        self
    }
}

/// A semantic tag of the Descriptor cluster's `TagList`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticTag {
    /// The vendor that defines the namespace; `None` for a standard namespace.
    pub manufacturer_code: Option<VendorId>,
    /// The namespace id.
    pub namespace: u8,
    /// The tag within the namespace.
    pub tag: u8,
    /// A label, where the namespace allows one.
    pub label: Option<String>,
}

impl SemanticTag {
    /// A tag of a standard namespace.
    #[must_use]
    pub const fn standard(namespace: u8, tag: u8) -> Self {
        Self {
            manufacturer_code: None,
            namespace,
            tag,
            label: None,
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The tag as the `SemanticTagStruct` value of the Descriptor cluster.
    #[must_use]
    pub fn to_value(&self) -> Value {
        use crate::ids::descriptor::{
            TAG_FIELD_LABEL, TAG_FIELD_MANUFACTURER_CODE, TAG_FIELD_NAMESPACE, TAG_FIELD_TAG,
        };
        let mut fields = alloc::vec![
            (
                TAG_FIELD_MANUFACTURER_CODE,
                Value::from(self.manufacturer_code.map(VendorId::raw)),
            ),
            (TAG_FIELD_NAMESPACE, Value::U8(self.namespace)),
            (TAG_FIELD_TAG, Value::U8(self.tag)),
        ];
        if let Some(label) = &self.label {
            fields.push((TAG_FIELD_LABEL, Value::from(label.as_str())));
        }
        Value::Struct(fields)
    }
}

/// A typed attribute of a bridged device.
///
/// Every variant names one attribute of one cluster a device kind serves; [`Attribute::path`]
/// gives the ids. Adapters use these to supply initial values and to report changes; the
/// [`Attribute::Raw`] escape addresses an attribute by its ids, for the extra clusters of a part
/// or for attributes without a typed name.
///
/// Measurement units are those of the Matter clusters: temperatures in hundredths of a degree
/// Celsius, humidity in hundredths of a percent, pressure in tenths of a kilopascal, illuminance
/// as `10000 * log10(lux) + 1`, colour temperature in mireds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Attribute {
    /// On/Off `OnOff`: whether the device is on.
    OnOff,
    /// On/Off `OnTime` in tenths of a second.
    OnTime,
    /// On/Off `OffWaitTime` in tenths of a second.
    OffWaitTime,
    /// On/Off `StartUpOnOff`: `0` off, `1` on, `2` toggle, null previous state.
    StartUpOnOff,
    /// Level Control `CurrentLevel`, `1..=254`, nullable.
    CurrentLevel,
    /// Level Control `RemainingTime` in tenths of a second.
    LevelRemainingTime,
    /// Level Control `OnLevel`, the level an `On` command sets, nullable.
    OnLevel,
    /// Level Control `Options` bitmap; bit 0 is `ExecuteIfOff`.
    LevelOptions,
    /// Level Control `StartUpCurrentLevel`: `0` minimum, `1..=254` that level, `255` or null
    /// the previous level.
    StartUpCurrentLevel,
    /// Color Control `CurrentHue`, `0..=254`.
    CurrentHue,
    /// Color Control `CurrentSaturation`, `0..=254`.
    CurrentSaturation,
    /// Color Control `RemainingTime` in tenths of a second.
    ColorRemainingTime,
    /// Color Control `CurrentX`, `0..=0xFEFF`.
    CurrentX,
    /// Color Control `CurrentY`, `0..=0xFEFF`.
    CurrentY,
    /// Color Control `ColorTemperatureMireds`.
    ColorTemperatureMireds,
    /// Color Control `ColorMode`: `0` hue and saturation, `1` x and y, `2` colour temperature.
    ColorMode,
    /// Color Control `Options` bitmap; bit 0 is `ExecuteIfOff`.
    ColorOptions,
    /// Color Control `ColorTempPhysicalMinMireds`: the coldest colour temperature the device
    /// can produce.
    ColorTempPhysicalMinMireds,
    /// Color Control `ColorTempPhysicalMaxMireds`: the warmest colour temperature the device
    /// can produce.
    ColorTempPhysicalMaxMireds,
    /// Color Control `StartUpColorTemperatureMireds`, nullable.
    StartUpColorTemperatureMireds,
    /// Boolean State `StateValue`: for a contact sensor, `true` means closed.
    StateValue,
    /// Temperature Measurement `MeasuredValue`, nullable.
    Temperature,
    /// Temperature Measurement `MinMeasuredValue`, nullable.
    TemperatureMin,
    /// Temperature Measurement `MaxMeasuredValue`, nullable.
    TemperatureMax,
    /// Relative Humidity Measurement `MeasuredValue`, nullable.
    Humidity,
    /// Relative Humidity Measurement `MinMeasuredValue`, nullable.
    HumidityMin,
    /// Relative Humidity Measurement `MaxMeasuredValue`, nullable.
    HumidityMax,
    /// Pressure Measurement `MeasuredValue`, nullable.
    Pressure,
    /// Pressure Measurement `MinMeasuredValue`, nullable.
    PressureMin,
    /// Pressure Measurement `MaxMeasuredValue`, nullable.
    PressureMax,
    /// Illuminance Measurement `MeasuredValue`, nullable.
    Illuminance,
    /// Illuminance Measurement `MinMeasuredValue`, nullable.
    IlluminanceMin,
    /// Illuminance Measurement `MaxMeasuredValue`, nullable.
    IlluminanceMax,
    /// Occupancy Sensing `Occupancy` bitmap; bit 0 is `Occupied`.
    Occupancy,
    /// Occupancy Sensing `OccupancySensorType`.
    OccupancySensorType,
    /// Occupancy Sensing `OccupancySensorTypeBitmap`.
    OccupancySensorTypeBitmap,
    /// Switch `NumberOfPositions`, at least 2.
    NumberOfPositions,
    /// Switch `CurrentPosition`.
    CurrentPosition,
    /// Switch `MultiPressMax`, at least 2.
    MultiPressMax,
    /// Identify `IdentifyTime` in seconds.
    IdentifyTime,
    /// Identify `IdentifyType`: `0` none, `1` light output, `2` visible indicator, `3` audible
    /// beep, `4` display, `5` actuator.
    IdentifyType,
    /// An attribute by its ids.
    Raw(ClusterId, AttributeId),
}

impl Attribute {
    /// The cluster and attribute id of the attribute.
    #[must_use]
    pub const fn path(self) -> (ClusterId, AttributeId) {
        match self {
            Self::OnOff => (on_off::CLUSTER, on_off::ON_OFF),
            Self::OnTime => (on_off::CLUSTER, on_off::ON_TIME),
            Self::OffWaitTime => (on_off::CLUSTER, on_off::OFF_WAIT_TIME),
            Self::StartUpOnOff => (on_off::CLUSTER, on_off::START_UP_ON_OFF),
            Self::CurrentLevel => (level_control::CLUSTER, level_control::CURRENT_LEVEL),
            Self::LevelRemainingTime => (level_control::CLUSTER, level_control::REMAINING_TIME),
            Self::OnLevel => (level_control::CLUSTER, level_control::ON_LEVEL),
            Self::LevelOptions => (level_control::CLUSTER, level_control::OPTIONS),
            Self::StartUpCurrentLevel => (
                level_control::CLUSTER,
                level_control::START_UP_CURRENT_LEVEL,
            ),
            Self::CurrentHue => (color_control::CLUSTER, color_control::CURRENT_HUE),
            Self::CurrentSaturation => (color_control::CLUSTER, color_control::CURRENT_SATURATION),
            Self::ColorRemainingTime => (color_control::CLUSTER, color_control::REMAINING_TIME),
            Self::CurrentX => (color_control::CLUSTER, color_control::CURRENT_X),
            Self::CurrentY => (color_control::CLUSTER, color_control::CURRENT_Y),
            Self::ColorTemperatureMireds => (
                color_control::CLUSTER,
                color_control::COLOR_TEMPERATURE_MIREDS,
            ),
            Self::ColorMode => (color_control::CLUSTER, color_control::COLOR_MODE),
            Self::ColorOptions => (color_control::CLUSTER, color_control::OPTIONS),
            Self::ColorTempPhysicalMinMireds => (
                color_control::CLUSTER,
                color_control::COLOR_TEMP_PHYSICAL_MIN_MIREDS,
            ),
            Self::ColorTempPhysicalMaxMireds => (
                color_control::CLUSTER,
                color_control::COLOR_TEMP_PHYSICAL_MAX_MIREDS,
            ),
            Self::StartUpColorTemperatureMireds => (
                color_control::CLUSTER,
                color_control::START_UP_COLOR_TEMPERATURE_MIREDS,
            ),
            Self::StateValue => (boolean_state::CLUSTER, boolean_state::STATE_VALUE),
            Self::Temperature => (
                temperature_measurement::CLUSTER,
                temperature_measurement::MEASURED_VALUE,
            ),
            Self::TemperatureMin => (
                temperature_measurement::CLUSTER,
                temperature_measurement::MIN_MEASURED_VALUE,
            ),
            Self::TemperatureMax => (
                temperature_measurement::CLUSTER,
                temperature_measurement::MAX_MEASURED_VALUE,
            ),
            Self::Humidity => (
                relative_humidity_measurement::CLUSTER,
                relative_humidity_measurement::MEASURED_VALUE,
            ),
            Self::HumidityMin => (
                relative_humidity_measurement::CLUSTER,
                relative_humidity_measurement::MIN_MEASURED_VALUE,
            ),
            Self::HumidityMax => (
                relative_humidity_measurement::CLUSTER,
                relative_humidity_measurement::MAX_MEASURED_VALUE,
            ),
            Self::Pressure => (
                pressure_measurement::CLUSTER,
                pressure_measurement::MEASURED_VALUE,
            ),
            Self::PressureMin => (
                pressure_measurement::CLUSTER,
                pressure_measurement::MIN_MEASURED_VALUE,
            ),
            Self::PressureMax => (
                pressure_measurement::CLUSTER,
                pressure_measurement::MAX_MEASURED_VALUE,
            ),
            Self::Illuminance => (
                illuminance_measurement::CLUSTER,
                illuminance_measurement::MEASURED_VALUE,
            ),
            Self::IlluminanceMin => (
                illuminance_measurement::CLUSTER,
                illuminance_measurement::MIN_MEASURED_VALUE,
            ),
            Self::IlluminanceMax => (
                illuminance_measurement::CLUSTER,
                illuminance_measurement::MAX_MEASURED_VALUE,
            ),
            Self::Occupancy => (occupancy_sensing::CLUSTER, occupancy_sensing::OCCUPANCY),
            Self::OccupancySensorType => (
                occupancy_sensing::CLUSTER,
                occupancy_sensing::OCCUPANCY_SENSOR_TYPE,
            ),
            Self::OccupancySensorTypeBitmap => (
                occupancy_sensing::CLUSTER,
                occupancy_sensing::OCCUPANCY_SENSOR_TYPE_BITMAP,
            ),
            Self::NumberOfPositions => (switch::CLUSTER, switch::NUMBER_OF_POSITIONS),
            Self::CurrentPosition => (switch::CLUSTER, switch::CURRENT_POSITION),
            Self::MultiPressMax => (switch::CLUSTER, switch::MULTI_PRESS_MAX),
            Self::IdentifyTime => (identify::CLUSTER, identify::IDENTIFY_TIME),
            Self::IdentifyType => (identify::CLUSTER, identify::IDENTIFY_TYPE),
            Self::Raw(cluster, attribute) => (cluster, attribute),
        }
    }

    /// Every typed attribute, for tables and tests.
    pub const TYPED: [Self; 41] = [
        Self::OnOff,
        Self::OnTime,
        Self::OffWaitTime,
        Self::StartUpOnOff,
        Self::CurrentLevel,
        Self::LevelRemainingTime,
        Self::OnLevel,
        Self::LevelOptions,
        Self::StartUpCurrentLevel,
        Self::CurrentHue,
        Self::CurrentSaturation,
        Self::ColorRemainingTime,
        Self::CurrentX,
        Self::CurrentY,
        Self::ColorTemperatureMireds,
        Self::ColorMode,
        Self::ColorOptions,
        Self::ColorTempPhysicalMinMireds,
        Self::ColorTempPhysicalMaxMireds,
        Self::StartUpColorTemperatureMireds,
        Self::StateValue,
        Self::Temperature,
        Self::TemperatureMin,
        Self::TemperatureMax,
        Self::Humidity,
        Self::HumidityMin,
        Self::HumidityMax,
        Self::Pressure,
        Self::PressureMin,
        Self::PressureMax,
        Self::Illuminance,
        Self::IlluminanceMin,
        Self::IlluminanceMax,
        Self::Occupancy,
        Self::OccupancySensorType,
        Self::OccupancySensorTypeBitmap,
        Self::NumberOfPositions,
        Self::CurrentPosition,
        Self::MultiPressMax,
        Self::IdentifyTime,
        Self::IdentifyType,
    ];

    /// The typed attribute for a cluster and attribute id, or [`Attribute::Raw`] when none
    /// exists.
    #[must_use]
    pub fn from_path(cluster: ClusterId, attribute: AttributeId) -> Self {
        Self::TYPED
            .into_iter()
            .find(|typed| typed.path() == (cluster, attribute))
            .unwrap_or(Self::Raw(cluster, attribute))
    }
}

impl fmt::Display for Attribute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Raw(cluster, attribute) => {
                write!(f, "attribute {attribute} of cluster {cluster}")
            }
            typed => write!(f, "{typed:?}"),
        }
    }
}

/// The values an adapter supplies when it adds a device.
///
/// A value is addressed by the part it belongs to and its typed attribute. Every attribute a
/// kind requires from the adapter must be present; every other attribute may be present and
/// overrides the kind's default. Values are checked against the attribute's type and range when
/// the device is added.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InitialValues {
    values: BTreeMap<(PartIndex, Attribute), Value>,
    reachable: Option<bool>,
}

impl InitialValues {
    /// No values.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a value for an attribute of a part.
    #[must_use]
    pub fn with(mut self, part: PartIndex, attribute: Attribute, value: impl Into<Value>) -> Self {
        self.values.insert((part, attribute), value.into());
        self
    }

    /// Adds a value for an attribute of the first part, for single-part devices.
    #[must_use]
    pub fn value(self, attribute: Attribute, value: impl Into<Value>) -> Self {
        self.with(0, attribute, value)
    }

    /// Sets whether the device is reachable when it is added; `true` when not set.
    #[must_use]
    pub fn reachable(mut self, reachable: bool) -> Self {
        self.reachable = Some(reachable);
        self
    }

    /// The value supplied for an attribute of a part.
    #[must_use]
    pub fn get(&self, part: PartIndex, attribute: Attribute) -> Option<&Value> {
        self.values.get(&(part, attribute))
    }

    /// Whether the device starts reachable.
    #[must_use]
    pub fn is_reachable(&self) -> bool {
        self.reachable.unwrap_or(true)
    }

    /// Every supplied value.
    pub fn iter(&self) -> impl Iterator<Item = (PartIndex, Attribute, &Value)> {
        self.values
            .iter()
            .map(|((part, attribute), value)| (*part, *attribute, value))
    }
}

/// Input an adapter reports for a device part that produces events: the presses of a generic
/// switch, or a raw event of an extra cluster.
///
/// Positions are those of the Switch cluster: `0` is the idle position of a momentary switch,
/// `1` and up are the pressed positions.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum DeviceEvent {
    /// The switch was pressed; `CurrentPosition` becomes `position`.
    InitialPress {
        /// The pressed position.
        position: u8,
    },
    /// The switch has been held long enough to count as a long press.
    LongPress {
        /// The pressed position.
        position: u8,
    },
    /// The switch was released after a short press; `CurrentPosition` returns to idle.
    ShortRelease {
        /// The position that was released.
        previous_position: u8,
    },
    /// The switch was released after a long press; `CurrentPosition` returns to idle.
    LongRelease {
        /// The position that was released.
        previous_position: u8,
    },
    /// Another press of a multi-press sequence was counted.
    MultiPressOngoing {
        /// The pressed position.
        position: u8,
        /// How many presses have been counted so far, at least 2.
        count: u8,
    },
    /// A multi-press sequence ended.
    MultiPressComplete {
        /// The position that was pressed.
        previous_position: u8,
        /// How many presses the sequence had.
        count: u8,
    },
    /// An event of an extra cluster, by its ids.
    Raw {
        /// The cluster.
        cluster: ClusterId,
        /// The event.
        event: EventId,
        /// The event fields.
        payload: Value,
    },
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;
    use alloc::vec;

    use matter_rs_engine::{AttributeId, ClusterId, Value, VendorId};

    use super::{Attribute, DeviceKey, InitialValues, SemanticTag};
    use crate::error::Error;

    #[test]
    fn device_key_is_validated() {
        assert_eq!(DeviceKey::new("lamp-1").unwrap().as_str(), "lamp-1");
        assert_eq!(DeviceKey::new("lamp-1").unwrap().to_string(), "lamp-1");
        assert!(matches!(DeviceKey::new(""), Err(Error::InvalidKey { .. })));
        assert!(matches!(
            DeviceKey::new("a\nb"),
            Err(Error::InvalidKey { .. })
        ));
        assert!(matches!(
            DeviceKey::new("k".repeat(65)),
            Err(Error::InvalidKey { .. })
        ));
        assert!(DeviceKey::try_from("k".repeat(64).as_str()).is_ok());
    }

    #[test]
    fn typed_attributes_round_trip_through_their_path() {
        for attribute in Attribute::TYPED {
            let (cluster, id) = attribute.path();
            assert_eq!(Attribute::from_path(cluster, id), attribute);
        }
        let raw = Attribute::from_path(ClusterId::new(0xFC01), AttributeId::new(7));
        assert_eq!(
            raw,
            Attribute::Raw(ClusterId::new(0xFC01), AttributeId::new(7))
        );
        assert_eq!(raw.to_string(), "attribute 0x0007 of cluster 0xFC01");
        assert_eq!(Attribute::OnOff.to_string(), "OnOff");
    }

    #[test]
    fn typed_attribute_paths_are_unique() {
        let typed = Attribute::TYPED;
        for (i, a) in typed.iter().enumerate() {
            for b in &typed[i + 1..] {
                assert_ne!(a.path(), b.path(), "{a:?} and {b:?} share a path");
            }
        }
    }

    #[test]
    fn initial_values_are_addressed_by_part_and_attribute() {
        let values = InitialValues::new()
            .value(Attribute::OnOff, true)
            .with(1, Attribute::Temperature, Value::I16(2100))
            .reachable(false);
        assert_eq!(values.get(0, Attribute::OnOff), Some(&Value::Bool(true)));
        assert_eq!(
            values.get(1, Attribute::Temperature),
            Some(&Value::I16(2100))
        );
        assert_eq!(values.get(1, Attribute::OnOff), None);
        assert!(!values.is_reachable());
        assert!(InitialValues::new().is_reachable());
        assert_eq!(values.iter().count(), 2);
    }

    #[test]
    fn semantic_tag_encodes_as_struct() {
        let tag = SemanticTag::standard(7, 2).with_label("Left");
        assert_eq!(
            tag.to_value(),
            Value::Struct(vec![
                (0, Value::Null),
                (1, Value::U8(7)),
                (2, Value::U8(2)),
                (3, Value::from("Left")),
            ])
        );
        let vendor = SemanticTag {
            manufacturer_code: Some(VendorId::new(0xFFF1)),
            namespace: 1,
            tag: 2,
            label: None,
        };
        assert_eq!(
            vendor.to_value(),
            Value::Struct(vec![
                (0, Value::U16(0xFFF1)),
                (1, Value::U8(1)),
                (2, Value::U8(2)),
            ])
        );
    }
}
