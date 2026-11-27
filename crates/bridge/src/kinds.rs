//! The device kinds the bridge serves and the clusters each kind requires.
//!
//! Every kind is one device type of the Matter 1.6 device library with the server clusters that
//! library makes mandatory, read from the `data_model/1.6` directory of
//! <https://github.com/project-chip/connectedhomeip> (`device_types/*.xml` for the device type
//! ids, revisions and cluster requirements; `clusters/*.xml` for the attributes, their defaults,
//! commands and events). Two mandatory clusters are left out on purpose: Groups and Scenes
//! Management need group addressing and scene tables inside the engine, which the engine facade
//! does not offer, and no ecosystem requires them of bridged devices.
//!
//! Every attribute a kind lists has either a default the cluster specification names or is
//! required from the adapter when the device is added; the bridge invents no value. The tables
//! below say which is which.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use matter_rs_engine::{ClusterId, CommandId, DeviceType, EventId, Kind, Value};

use crate::ids::{
    boolean_state, color_control, device_type, identify, level_control, on_off,
    relative_humidity_measurement, temperature_measurement,
};
use crate::model::Attribute;

/// A kind of bridged device: a device type with its mandatory clusters.
///
/// | Kind | Device type | Clusters besides Identify | Required from the adapter |
/// |---|---|---|---|
/// | [`OnOffLight`](Self::OnOffLight) | On/Off Light `0x0100` rev 3 | On/Off (Lighting) | `OnOff` |
/// | [`DimmableLight`](Self::DimmableLight) | Dimmable Light `0x0101` rev 3 | On/Off, Level Control (`OnOff`, Lighting) | `OnOff`, `CurrentLevel` |
/// | [`ColorTemperatureLight`](Self::ColorTemperatureLight) | Color Temperature Light `0x010C` rev 4 | On/Off, Level Control, Color Control (CT) | `OnOff`, `CurrentLevel`, `ColorTemperatureMireds`, `ColorTempPhysicalMinMireds`, `ColorTempPhysicalMaxMireds` |
/// | [`ExtendedColorLight`](Self::ExtendedColorLight) | Extended Color Light `0x010D` rev 4 | On/Off, Level Control, Color Control (HS, XY, CT) | as the colour temperature light |
/// | [`OnOffPlugInUnit`](Self::OnOffPlugInUnit) | On/Off Plug-in Unit `0x010A` rev 4 | On/Off (Lighting) | `OnOff` |
/// | [`ContactSensor`](Self::ContactSensor) | Contact Sensor `0x0015` rev 2 | Boolean State | `StateValue` |
/// | [`TemperatureSensor`](Self::TemperatureSensor) | Temperature Sensor `0x0302` rev 3 | Temperature Measurement | `Temperature` |
/// | [`HumiditySensor`](Self::HumiditySensor) | Humidity Sensor `0x0307` rev 3 | Relative Humidity Measurement | `Humidity` |
///
/// Every kind also serves the Identify cluster (`IdentifyTime` 0, `IdentifyType` none), and the
/// top endpoint of every device carries the Bridged Node device type with the Bridged Device
/// Basic Information cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum DeviceKind {
    /// A light that is on or off.
    OnOffLight,
    /// A light with a brightness level.
    DimmableLight,
    /// A dimmable light with an adjustable colour temperature.
    ColorTemperatureLight,
    /// A dimmable light with colour (hue and saturation, x and y) and colour temperature.
    ExtendedColorLight,
    /// A mains socket that is on or off.
    OnOffPlugInUnit,
    /// A sensor with a boolean state, such as a door or window contact.
    ContactSensor,
    /// A temperature sensor.
    TemperatureSensor,
    /// A relative humidity sensor.
    HumiditySensor,
}

impl DeviceKind {
    /// Every kind, in the order of the table.
    pub const ALL: [Self; 8] = [
        Self::OnOffLight,
        Self::DimmableLight,
        Self::ColorTemperatureLight,
        Self::ExtendedColorLight,
        Self::OnOffPlugInUnit,
        Self::ContactSensor,
        Self::TemperatureSensor,
        Self::HumiditySensor,
    ];

    /// The device type and revision of the kind.
    #[must_use]
    pub const fn device_type(self) -> DeviceType {
        let (id, revision) = match self {
            Self::OnOffLight => device_type::ON_OFF_LIGHT,
            Self::DimmableLight => device_type::DIMMABLE_LIGHT,
            Self::ColorTemperatureLight => device_type::COLOR_TEMPERATURE_LIGHT,
            Self::ExtendedColorLight => device_type::EXTENDED_COLOR_LIGHT,
            Self::OnOffPlugInUnit => device_type::ON_OFF_PLUG_IN_UNIT,
            Self::ContactSensor => device_type::CONTACT_SENSOR,
            Self::TemperatureSensor => device_type::TEMPERATURE_SENSOR,
            Self::HumiditySensor => device_type::HUMIDITY_SENSOR,
        };
        DeviceType { id, revision }
    }

    /// The kebab-case name of the kind, as a configuration would spell it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::OnOffLight => "on-off-light",
            Self::DimmableLight => "dimmable-light",
            Self::ColorTemperatureLight => "color-temperature-light",
            Self::ExtendedColorLight => "extended-color-light",
            Self::OnOffPlugInUnit => "on-off-plug-in-unit",
            Self::ContactSensor => "contact-sensor",
            Self::TemperatureSensor => "temperature-sensor",
            Self::HumiditySensor => "humidity-sensor",
        }
    }

    /// The kind with this [`name`](Self::name).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name() == name)
    }

    /// The attributes an adapter must supply when it adds a device of this kind.
    #[must_use]
    pub fn required_attributes(self) -> Vec<Attribute> {
        self.clusters()
            .iter()
            .flat_map(|cluster| cluster.attributes.iter())
            .filter(|attribute| matches!(attribute.initial, Initial::Required))
            .map(|attribute| attribute.attribute)
            .collect()
    }

    /// The clusters of the kind, the Identify cluster first.
    pub(crate) fn clusters(self) -> Vec<ClusterSpec> {
        let mut clusters = vec![identify_cluster()];
        match self {
            Self::OnOffLight | Self::OnOffPlugInUnit => clusters.push(on_off_cluster()),
            Self::DimmableLight => {
                clusters.push(on_off_cluster());
                clusters.push(level_control_cluster());
            }
            Self::ColorTemperatureLight => {
                clusters.push(on_off_cluster());
                clusters.push(level_control_cluster());
                clusters.push(color_control_cluster(
                    color_control::FEATURE_COLOR_TEMPERATURE,
                ));
            }
            Self::ExtendedColorLight => {
                clusters.push(on_off_cluster());
                clusters.push(level_control_cluster());
                clusters.push(color_control_cluster(
                    color_control::FEATURE_HUE_SATURATION
                        | color_control::FEATURE_XY
                        | color_control::FEATURE_COLOR_TEMPERATURE,
                ));
            }
            Self::ContactSensor => clusters.push(boolean_state_cluster()),
            Self::TemperatureSensor => clusters.push(temperature_cluster()),
            Self::HumiditySensor => clusters.push(humidity_cluster()),
        }
        clusters
    }
}

/// Where the initial value of an attribute comes from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Initial {
    /// The default the cluster specification names.
    Default(Value),
    /// The adapter supplies it when the device is added.
    Required,
}

/// Whether and how a controller may write the attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Read-only for controllers; the adapter sets it.
    ReadOnly,
    /// Writable; the bridge keeps the written value while it runs.
    Writable,
    /// Writable; the bridge keeps the written value across restarts.
    Persisted,
}

/// One attribute a kind serves: its type, range, initial value and access.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AttributeSpec {
    /// The attribute.
    pub attribute: Attribute,
    /// The value kind; `None` for an attribute of an extra cluster, whose type the bridge does
    /// not know.
    pub kind: Option<Kind>,
    /// Whether null is a valid value.
    pub nullable: bool,
    /// The inclusive range of an integer attribute.
    pub range: Option<(i64, i64)>,
    /// The initial value.
    pub initial: Initial,
    /// The access controllers have.
    pub access: Access,
}

impl AttributeSpec {
    const fn new(attribute: Attribute, kind: Kind, initial: Initial, access: Access) -> Self {
        Self {
            attribute,
            kind: Some(kind),
            nullable: false,
            range: None,
            initial,
            access,
        }
    }

    const fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    const fn range(mut self, min: i64, max: i64) -> Self {
        self.range = Some((min, max));
        self
    }

    /// An attribute of an extra cluster: any kind, every value accepted, written values go to
    /// the adapter.
    pub(crate) const fn raw(attribute: Attribute) -> Self {
        Self {
            attribute,
            kind: None,
            nullable: true,
            range: None,
            initial: Initial::Required,
            access: Access::ReadOnly,
        }
    }

    /// Checks a value against the type, nullability and range of the attribute and returns it
    /// in the attribute's integer width.
    ///
    /// Integers arrive in whatever width the sender chose (a Matter client encodes `5` in one
    /// byte for a 16-bit attribute); the result carries the attribute's width.
    pub(crate) fn check(&self, value: &Value) -> Result<Value, String> {
        let Some(kind) = self.kind else {
            return Ok(value.clone());
        };
        if value.is_null() {
            return if self.nullable {
                Ok(Value::Null)
            } else {
                Err(String::from("null is not allowed"))
            };
        }
        let normalised = normalise(kind, value)?;
        if let Some((min, max)) = self.range {
            let within = match integer(&normalised) {
                Some(n) => n >= min && n <= max,
                None => true,
            };
            if !within {
                return Err(alloc::format!("outside {min}..={max}"));
            }
        }
        Ok(normalised)
    }
}

/// Converts `value` to `kind`, accepting any integer width of the same signedness that fits.
fn normalise(kind: Kind, value: &Value) -> Result<Value, String> {
    let mismatch = || alloc::format!("expected {kind}, found {}", value.kind());
    let value = value.clone();
    let converted = match kind {
        Kind::Bool => Value::Bool(bool::try_from(value).map_err(|_| mismatch())?),
        Kind::U8 => Value::U8(u8::try_from(value).map_err(|_| mismatch())?),
        Kind::U16 => Value::U16(u16::try_from(value).map_err(|_| mismatch())?),
        Kind::U32 => Value::U32(u32::try_from(value).map_err(|_| mismatch())?),
        Kind::U64 => Value::U64(u64::try_from(value).map_err(|_| mismatch())?),
        Kind::I8 => Value::I8(i8::try_from(value).map_err(|_| mismatch())?),
        Kind::I16 => Value::I16(i16::try_from(value).map_err(|_| mismatch())?),
        Kind::I32 => Value::I32(i32::try_from(value).map_err(|_| mismatch())?),
        Kind::I64 => Value::I64(i64::try_from(value).map_err(|_| mismatch())?),
        Kind::Utf8 => Value::Utf8(String::try_from(value).map_err(|_| mismatch())?),
        other => {
            if value.kind() == other {
                value
            } else {
                return Err(mismatch());
            }
        }
    };
    Ok(converted)
}

/// The value of an integer as `i64`; `None` for other kinds (and for `u64` beyond `i64`).
fn integer(value: &Value) -> Option<i64> {
    match value {
        Value::U8(v) => Some(i64::from(*v)),
        Value::U16(v) => Some(i64::from(*v)),
        Value::U32(v) => Some(i64::from(*v)),
        Value::U64(v) => i64::try_from(*v).ok(),
        Value::I8(v) => Some(i64::from(*v)),
        Value::I16(v) => Some(i64::from(*v)),
        Value::I32(v) => Some(i64::from(*v)),
        Value::I64(v) => Some(*v),
        _ => None,
    }
}

/// One cluster a kind serves.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClusterSpec {
    /// The cluster id.
    pub id: ClusterId,
    /// The cluster revision.
    pub revision: u16,
    /// The feature map.
    pub feature_map: u32,
    /// The attributes, without the global ones.
    pub attributes: Vec<AttributeSpec>,
    /// The accepted commands.
    pub commands: Vec<CommandId>,
    /// The events the cluster emits.
    pub events: Vec<EventId>,
}

impl ClusterSpec {
    /// The specification of an attribute of this cluster.
    pub(crate) fn attribute(&self, attribute: Attribute) -> Option<&AttributeSpec> {
        self.attributes.iter().find(|a| a.attribute == attribute)
    }
}

fn identify_cluster() -> ClusterSpec {
    ClusterSpec {
        id: identify::CLUSTER,
        revision: identify::REVISION,
        feature_map: 0,
        attributes: vec![
            AttributeSpec::new(
                Attribute::IdentifyTime,
                Kind::U16,
                Initial::Default(Value::U16(0)),
                Access::Writable,
            ),
            AttributeSpec::new(
                Attribute::IdentifyType,
                Kind::U8,
                Initial::Default(Value::U8(identify::IDENTIFY_TYPE_NONE)),
                Access::ReadOnly,
            )
            .range(0, 5),
        ],
        commands: vec![identify::IDENTIFY, identify::TRIGGER_EFFECT],
        events: vec![],
    }
}

fn on_off_cluster() -> ClusterSpec {
    ClusterSpec {
        id: on_off::CLUSTER,
        revision: on_off::REVISION,
        feature_map: on_off::FEATURE_LIGHTING,
        attributes: vec![
            AttributeSpec::new(
                Attribute::OnOff,
                Kind::Bool,
                Initial::Required,
                Access::ReadOnly,
            ),
            AttributeSpec::new(
                Attribute::Raw(on_off::CLUSTER, on_off::GLOBAL_SCENE_CONTROL),
                Kind::Bool,
                Initial::Default(Value::Bool(true)),
                Access::ReadOnly,
            ),
            AttributeSpec::new(
                Attribute::OnTime,
                Kind::U16,
                Initial::Default(Value::U16(0)),
                Access::Writable,
            ),
            AttributeSpec::new(
                Attribute::OffWaitTime,
                Kind::U16,
                Initial::Default(Value::U16(0)),
                Access::Writable,
            ),
            AttributeSpec::new(
                Attribute::StartUpOnOff,
                Kind::U8,
                Initial::Default(Value::Null),
                Access::Persisted,
            )
            .nullable()
            .range(0, 2),
        ],
        commands: vec![
            on_off::OFF,
            on_off::ON,
            on_off::TOGGLE,
            on_off::OFF_WITH_EFFECT,
            on_off::ON_WITH_RECALL_GLOBAL_SCENE,
            on_off::ON_WITH_TIMED_OFF,
        ],
        events: vec![],
    }
}

fn level_control_cluster() -> ClusterSpec {
    let min = i64::from(level_control::LIGHTING_MIN_LEVEL);
    let max = i64::from(level_control::LIGHTING_MAX_LEVEL);
    ClusterSpec {
        id: level_control::CLUSTER,
        revision: level_control::REVISION,
        feature_map: level_control::FEATURE_ON_OFF | level_control::FEATURE_LIGHTING,
        attributes: vec![
            AttributeSpec::new(
                Attribute::CurrentLevel,
                Kind::U8,
                Initial::Required,
                Access::ReadOnly,
            )
            .nullable()
            .range(min, max),
            AttributeSpec::new(
                Attribute::LevelRemainingTime,
                Kind::U16,
                Initial::Default(Value::U16(0)),
                Access::ReadOnly,
            ),
            AttributeSpec::new(
                Attribute::Raw(level_control::CLUSTER, level_control::MIN_LEVEL),
                Kind::U8,
                Initial::Default(Value::U8(level_control::LIGHTING_MIN_LEVEL)),
                Access::ReadOnly,
            ),
            AttributeSpec::new(
                Attribute::Raw(level_control::CLUSTER, level_control::MAX_LEVEL),
                Kind::U8,
                Initial::Default(Value::U8(level_control::LIGHTING_MAX_LEVEL)),
                Access::ReadOnly,
            ),
            AttributeSpec::new(
                Attribute::LevelOptions,
                Kind::U8,
                Initial::Default(Value::U8(0)),
                Access::Persisted,
            ),
            AttributeSpec::new(
                Attribute::OnLevel,
                Kind::U8,
                Initial::Default(Value::Null),
                Access::Persisted,
            )
            .nullable()
            .range(min, max),
            AttributeSpec::new(
                Attribute::StartUpCurrentLevel,
                Kind::U8,
                Initial::Default(Value::Null),
                Access::Persisted,
            )
            .nullable(),
        ],
        commands: vec![
            level_control::MOVE_TO_LEVEL,
            level_control::MOVE,
            level_control::STEP,
            level_control::STOP,
            level_control::MOVE_TO_LEVEL_WITH_ON_OFF,
            level_control::MOVE_WITH_ON_OFF,
            level_control::STEP_WITH_ON_OFF,
            level_control::STOP_WITH_ON_OFF,
        ],
        events: vec![],
    }
}

/// The Color Control cluster with the given features. The colour mode defaults to colour
/// temperature, because that is the one colour the adapter must supply; an adapter that knows
/// better overrides `ColorMode`.
fn color_control_cluster(features: u32) -> ClusterSpec {
    let max_coordinate = i64::from(color_control::MAX_COLOR_COORDINATE);
    let max_hue_saturation = i64::from(color_control::MAX_HUE_SATURATION);
    let mut attributes = vec![
        AttributeSpec::new(
            Attribute::ColorRemainingTime,
            Kind::U16,
            Initial::Default(Value::U16(0)),
            Access::ReadOnly,
        ),
        AttributeSpec::new(
            Attribute::ColorMode,
            Kind::U8,
            Initial::Default(Value::U8(color_control::MODE_COLOR_TEMPERATURE)),
            Access::ReadOnly,
        )
        .range(0, 2),
        AttributeSpec::new(
            Attribute::ColorOptions,
            Kind::U8,
            Initial::Default(Value::U8(0)),
            Access::Persisted,
        ),
        AttributeSpec::new(
            Attribute::Raw(color_control::CLUSTER, color_control::NUMBER_OF_PRIMARIES),
            Kind::U8,
            Initial::Default(Value::Null),
            Access::ReadOnly,
        )
        .nullable()
        .range(0, 6),
        AttributeSpec::new(
            Attribute::Raw(color_control::CLUSTER, color_control::ENHANCED_COLOR_MODE),
            Kind::U8,
            Initial::Default(Value::U8(color_control::MODE_COLOR_TEMPERATURE)),
            Access::ReadOnly,
        )
        .range(0, 3),
        AttributeSpec::new(
            Attribute::Raw(color_control::CLUSTER, color_control::COLOR_CAPABILITIES),
            Kind::U16,
            Initial::Default(Value::U16(u16::try_from(features).unwrap_or(0))),
            Access::ReadOnly,
        ),
    ];
    let mut commands = vec![color_control::STOP_MOVE_STEP];
    if features & color_control::FEATURE_HUE_SATURATION != 0 {
        attributes.push(
            AttributeSpec::new(
                Attribute::CurrentHue,
                Kind::U8,
                Initial::Default(Value::U8(0)),
                Access::ReadOnly,
            )
            .range(0, max_hue_saturation),
        );
        attributes.push(
            AttributeSpec::new(
                Attribute::CurrentSaturation,
                Kind::U8,
                Initial::Default(Value::U8(0)),
                Access::ReadOnly,
            )
            .range(0, max_hue_saturation),
        );
        commands.extend([
            color_control::MOVE_TO_HUE,
            color_control::MOVE_HUE,
            color_control::STEP_HUE,
            color_control::MOVE_TO_SATURATION,
            color_control::MOVE_SATURATION,
            color_control::STEP_SATURATION,
            color_control::MOVE_TO_HUE_AND_SATURATION,
        ]);
    }
    if features & color_control::FEATURE_XY != 0 {
        attributes.push(
            AttributeSpec::new(
                Attribute::CurrentX,
                Kind::U16,
                Initial::Default(Value::U16(color_control::DEFAULT_CURRENT_X)),
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        attributes.push(
            AttributeSpec::new(
                Attribute::CurrentY,
                Kind::U16,
                Initial::Default(Value::U16(color_control::DEFAULT_CURRENT_Y)),
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        commands.extend([
            color_control::MOVE_TO_COLOR,
            color_control::MOVE_COLOR,
            color_control::STEP_COLOR,
        ]);
    }
    if features & color_control::FEATURE_COLOR_TEMPERATURE != 0 {
        attributes.push(
            AttributeSpec::new(
                Attribute::ColorTemperatureMireds,
                Kind::U16,
                Initial::Required,
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        attributes.push(
            AttributeSpec::new(
                Attribute::ColorTempPhysicalMinMireds,
                Kind::U16,
                Initial::Required,
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        attributes.push(
            AttributeSpec::new(
                Attribute::ColorTempPhysicalMaxMireds,
                Kind::U16,
                Initial::Required,
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        // Derived from `ColorTempPhysicalMinMireds` when the device is added; the default here
        // is only a placeholder of the right type.
        attributes.push(
            AttributeSpec::new(
                Attribute::Raw(
                    color_control::CLUSTER,
                    color_control::COUPLE_COLOR_TEMP_TO_LEVEL_MIN_MIREDS,
                ),
                Kind::U16,
                Initial::Default(Value::U16(0)),
                Access::ReadOnly,
            )
            .range(0, max_coordinate),
        );
        attributes.push(
            AttributeSpec::new(
                Attribute::StartUpColorTemperatureMireds,
                Kind::U16,
                Initial::Default(Value::Null),
                Access::Persisted,
            )
            .nullable()
            .range(i64::from(color_control::MIN_MIREDS), max_coordinate),
        );
        commands.extend([
            color_control::MOVE_TO_COLOR_TEMPERATURE,
            color_control::MOVE_COLOR_TEMPERATURE,
            color_control::STEP_COLOR_TEMPERATURE,
        ]);
    }
    ClusterSpec {
        id: color_control::CLUSTER,
        revision: color_control::REVISION,
        feature_map: features,
        attributes,
        commands,
        events: vec![],
    }
}

fn boolean_state_cluster() -> ClusterSpec {
    ClusterSpec {
        id: boolean_state::CLUSTER,
        revision: boolean_state::REVISION,
        feature_map: 0,
        attributes: vec![AttributeSpec::new(
            Attribute::StateValue,
            Kind::Bool,
            Initial::Required,
            Access::ReadOnly,
        )],
        commands: vec![],
        events: vec![boolean_state::STATE_CHANGE],
    }
}

/// A measurement cluster: a nullable measured value the adapter supplies and nullable limits
/// that default to null, which the cluster specifications define as "not available".
fn measurement_cluster(
    id: ClusterId,
    revision: u16,
    kind: Kind,
    range: (i64, i64),
    measured: Attribute,
    min: Attribute,
    max: Attribute,
) -> ClusterSpec {
    ClusterSpec {
        id,
        revision,
        feature_map: 0,
        attributes: vec![
            AttributeSpec::new(measured, kind, Initial::Required, Access::ReadOnly)
                .nullable()
                .range(range.0, range.1),
            AttributeSpec::new(min, kind, Initial::Default(Value::Null), Access::ReadOnly)
                .nullable()
                .range(range.0, range.1),
            AttributeSpec::new(max, kind, Initial::Default(Value::Null), Access::ReadOnly)
                .nullable()
                .range(range.0, range.1),
        ],
        commands: vec![],
        events: vec![],
    }
}

fn temperature_cluster() -> ClusterSpec {
    measurement_cluster(
        temperature_measurement::CLUSTER,
        temperature_measurement::REVISION,
        Kind::I16,
        (-27315, 32767),
        Attribute::Temperature,
        Attribute::TemperatureMin,
        Attribute::TemperatureMax,
    )
}

fn humidity_cluster() -> ClusterSpec {
    measurement_cluster(
        relative_humidity_measurement::CLUSTER,
        relative_humidity_measurement::REVISION,
        Kind::U16,
        (0, 10000),
        Attribute::Humidity,
        Attribute::HumidityMin,
        Attribute::HumidityMax,
    )
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use matter_rs_engine::{Kind, Value};

    use super::{Access, AttributeSpec, DeviceKind, Initial};
    use crate::ids::{color_control, identify};
    use crate::model::Attribute;

    #[test]
    fn names_round_trip() {
        for kind in DeviceKind::ALL {
            assert_eq!(DeviceKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(DeviceKind::from_name("toaster"), None);
    }

    #[test]
    fn every_kind_lists_identify_first_and_unique_clusters() {
        for kind in DeviceKind::ALL {
            let clusters = kind.clusters();
            assert_eq!(clusters[0].id, identify::CLUSTER, "{kind:?}");
            for (i, cluster) in clusters.iter().enumerate() {
                assert!(
                    !clusters[..i].iter().any(|c| c.id == cluster.id),
                    "{kind:?} lists cluster {} twice",
                    cluster.id
                );
                for (j, attribute) in cluster.attributes.iter().enumerate() {
                    let (cluster_id, _) = attribute.attribute.path();
                    assert_eq!(
                        cluster_id, cluster.id,
                        "{kind:?}: {:?}",
                        attribute.attribute
                    );
                    assert!(
                        !cluster.attributes[..j]
                            .iter()
                            .any(|a| a.attribute == attribute.attribute),
                        "{kind:?} lists {:?} twice",
                        attribute.attribute
                    );
                }
            }
        }
    }

    #[test]
    fn defaults_fit_their_own_specification() {
        for kind in DeviceKind::ALL {
            for cluster in kind.clusters() {
                for attribute in &cluster.attributes {
                    if let Initial::Default(value) = &attribute.initial {
                        attribute.check(value).unwrap_or_else(|reason| {
                            panic!("{kind:?}: default of {:?}: {reason}", attribute.attribute)
                        });
                    }
                }
            }
        }
    }

    #[test]
    fn required_attributes_match_the_table() {
        assert_eq!(
            DeviceKind::OnOffLight.required_attributes(),
            vec![Attribute::OnOff]
        );
        assert_eq!(
            DeviceKind::DimmableLight.required_attributes(),
            vec![Attribute::OnOff, Attribute::CurrentLevel]
        );
        assert_eq!(
            DeviceKind::ColorTemperatureLight.required_attributes(),
            vec![
                Attribute::OnOff,
                Attribute::CurrentLevel,
                Attribute::ColorTemperatureMireds,
                Attribute::ColorTempPhysicalMinMireds,
                Attribute::ColorTempPhysicalMaxMireds,
            ]
        );
        assert_eq!(
            DeviceKind::ExtendedColorLight.required_attributes(),
            DeviceKind::ColorTemperatureLight.required_attributes()
        );
        assert_eq!(
            DeviceKind::ContactSensor.required_attributes(),
            vec![Attribute::StateValue]
        );
        assert_eq!(
            DeviceKind::TemperatureSensor.required_attributes(),
            vec![Attribute::Temperature]
        );
        assert_eq!(
            DeviceKind::HumiditySensor.required_attributes(),
            vec![Attribute::Humidity]
        );
    }

    #[test]
    fn color_features_select_attributes_and_commands() {
        let ct = DeviceKind::ColorTemperatureLight.clusters();
        let ct = ct.iter().find(|c| c.id == color_control::CLUSTER).unwrap();
        assert_eq!(ct.feature_map, color_control::FEATURE_COLOR_TEMPERATURE);
        assert!(ct.attribute(Attribute::CurrentHue).is_none());
        assert!(ct.attribute(Attribute::ColorTemperatureMireds).is_some());
        assert!(!ct.commands.contains(&color_control::MOVE_TO_HUE));
        assert!(
            ct.commands
                .contains(&color_control::MOVE_TO_COLOR_TEMPERATURE)
        );

        let extended = DeviceKind::ExtendedColorLight.clusters();
        let extended = extended
            .iter()
            .find(|c| c.id == color_control::CLUSTER)
            .unwrap();
        assert_eq!(extended.feature_map, 0x19);
        assert!(extended.attribute(Attribute::CurrentHue).is_some());
        assert!(extended.attribute(Attribute::CurrentX).is_some());
        assert!(extended.commands.contains(&color_control::MOVE_TO_COLOR));
        assert_eq!(
            extended
                .attribute(Attribute::Raw(
                    color_control::CLUSTER,
                    color_control::COLOR_CAPABILITIES
                ))
                .unwrap()
                .initial,
            Initial::Default(Value::U16(0x19))
        );
    }

    #[test]
    fn check_normalises_width_and_enforces_range_and_nullability() {
        let level = AttributeSpec::new(
            Attribute::CurrentLevel,
            Kind::U8,
            Initial::Required,
            Access::ReadOnly,
        )
        .nullable()
        .range(1, 254);
        assert_eq!(level.check(&Value::U16(7)), Ok(Value::U8(7)));
        assert_eq!(level.check(&Value::Null), Ok(Value::Null));
        assert!(level.check(&Value::U8(0)).unwrap_err().contains("1..=254"));
        assert!(level.check(&Value::U16(300)).unwrap_err().contains("u8"));
        assert!(level.check(&Value::I8(3)).unwrap_err().contains("i8"));
        assert!(
            level
                .check(&Value::Bool(true))
                .unwrap_err()
                .contains("bool")
        );

        let on = AttributeSpec::new(
            Attribute::OnOff,
            Kind::Bool,
            Initial::Required,
            Access::ReadOnly,
        );
        assert!(on.check(&Value::Null).unwrap_err().contains("null"));
        assert_eq!(on.check(&Value::Bool(true)), Ok(Value::Bool(true)));

        let raw = AttributeSpec::raw(Attribute::Raw(
            color_control::CLUSTER,
            identify::IDENTIFY_TIME,
        ));
        assert_eq!(raw.check(&Value::List(vec![])), Ok(Value::List(vec![])));
    }
}
