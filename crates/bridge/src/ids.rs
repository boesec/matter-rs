//! The Matter ids the bridge role knows: clusters, attributes, commands, events and device
//! types.
//!
//! The ids and revisions are those of the Matter 1.6 data model, read from the `data_model/1.6`
//! directory of <https://github.com/project-chip/connectedhomeip> (`clusters/*.xml`,
//! `device_types/*.xml`, `namespaces/*.xml`). Everything above this module works with these
//! constants; adapters never see them unless they use the raw escapes.

use matter_rs_engine::{AttributeId, ClusterId, CommandId, DeviceTypeId, EventId};

/// Device type ids and revisions (`device_types/*.xml`).
pub mod device_type {
    use super::DeviceTypeId;

    /// Bridged Node, carried by the top endpoint of every bridged device.
    pub const BRIDGED_NODE: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0013), 3);
    /// On/Off Light.
    pub const ON_OFF_LIGHT: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0100), 3);
    /// Dimmable Light.
    pub const DIMMABLE_LIGHT: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0101), 3);
    /// Color Temperature Light.
    pub const COLOR_TEMPERATURE_LIGHT: (DeviceTypeId, u16) = (DeviceTypeId::new(0x010C), 4);
    /// Extended Color Light.
    pub const EXTENDED_COLOR_LIGHT: (DeviceTypeId, u16) = (DeviceTypeId::new(0x010D), 4);
    /// On/Off Plug-in Unit.
    pub const ON_OFF_PLUG_IN_UNIT: (DeviceTypeId, u16) = (DeviceTypeId::new(0x010A), 4);
    /// Contact Sensor.
    pub const CONTACT_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0015), 2);
    /// Temperature Sensor.
    pub const TEMPERATURE_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0302), 3);
    /// Humidity Sensor.
    pub const HUMIDITY_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0307), 3);
    /// Pressure Sensor.
    pub const PRESSURE_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0305), 3);
    /// Light Sensor.
    pub const LIGHT_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0106), 4);
    /// Occupancy Sensor.
    pub const OCCUPANCY_SENSOR: (DeviceTypeId, u16) = (DeviceTypeId::new(0x0107), 4);
    /// Generic Switch.
    pub const GENERIC_SWITCH: (DeviceTypeId, u16) = (DeviceTypeId::new(0x000F), 3);
}

/// Identify cluster (`clusters/Identify.xml`, revision 6).
pub mod identify {
    use super::{AttributeId, ClusterId, CommandId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0003);
    /// The cluster revision.
    pub const REVISION: u16 = 6;
    /// `IdentifyTime`, seconds left of identification, writable.
    pub const IDENTIFY_TIME: AttributeId = AttributeId::new(0x0000);
    /// `IdentifyType`, how the device shows identification.
    pub const IDENTIFY_TYPE: AttributeId = AttributeId::new(0x0001);
    /// `IdentifyType` value `None`: the device has no way to show identification.
    pub const IDENTIFY_TYPE_NONE: u8 = 0;
    /// `Identify` command: identify for `IdentifyTime` seconds.
    pub const IDENTIFY: CommandId = CommandId::new(0x00);
    /// `TriggerEffect` command: show an identification effect.
    pub const TRIGGER_EFFECT: CommandId = CommandId::new(0x40);
}

/// On/Off cluster (`clusters/OnOff.xml`, revision 6).
pub mod on_off {
    use super::{AttributeId, ClusterId, CommandId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0006);
    /// The cluster revision.
    pub const REVISION: u16 = 6;
    /// Feature `Lighting` (bit 0), required of every lighting device type and of the plug-in
    /// unit.
    pub const FEATURE_LIGHTING: u32 = 1 << 0;
    /// `OnOff`.
    pub const ON_OFF: AttributeId = AttributeId::new(0x0000);
    /// `GlobalSceneControl`.
    pub const GLOBAL_SCENE_CONTROL: AttributeId = AttributeId::new(0x4000);
    /// `OnTime`, writable.
    pub const ON_TIME: AttributeId = AttributeId::new(0x4001);
    /// `OffWaitTime`, writable.
    pub const OFF_WAIT_TIME: AttributeId = AttributeId::new(0x4002);
    /// `StartUpOnOff`, writable, nullable.
    pub const START_UP_ON_OFF: AttributeId = AttributeId::new(0x4003);
    /// `StartUpOnOff` value `Off`.
    pub const START_UP_OFF: u8 = 0;
    /// `StartUpOnOff` value `On`.
    pub const START_UP_ON: u8 = 1;
    /// `StartUpOnOff` value `Toggle`.
    pub const START_UP_TOGGLE: u8 = 2;
    /// `Off` command.
    pub const OFF: CommandId = CommandId::new(0x00);
    /// `On` command.
    pub const ON: CommandId = CommandId::new(0x01);
    /// `Toggle` command.
    pub const TOGGLE: CommandId = CommandId::new(0x02);
    /// `OffWithEffect` command.
    pub const OFF_WITH_EFFECT: CommandId = CommandId::new(0x40);
    /// `OnWithRecallGlobalScene` command.
    pub const ON_WITH_RECALL_GLOBAL_SCENE: CommandId = CommandId::new(0x41);
    /// `OnWithTimedOff` command.
    pub const ON_WITH_TIMED_OFF: CommandId = CommandId::new(0x42);
}

/// Level Control cluster (`clusters/LevelControl.xml`, revision 7).
pub mod level_control {
    use super::{AttributeId, ClusterId, CommandId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0008);
    /// The cluster revision.
    pub const REVISION: u16 = 7;
    /// Feature `OnOff` (bit 0): the level is coupled to the On/Off cluster.
    pub const FEATURE_ON_OFF: u32 = 1 << 0;
    /// Feature `Lighting` (bit 1).
    pub const FEATURE_LIGHTING: u32 = 1 << 1;
    /// `CurrentLevel`, nullable.
    pub const CURRENT_LEVEL: AttributeId = AttributeId::new(0x0000);
    /// `RemainingTime`, tenths of a second.
    pub const REMAINING_TIME: AttributeId = AttributeId::new(0x0001);
    /// `MinLevel`.
    pub const MIN_LEVEL: AttributeId = AttributeId::new(0x0002);
    /// `MaxLevel`.
    pub const MAX_LEVEL: AttributeId = AttributeId::new(0x0003);
    /// `Options`, writable bitmap.
    pub const OPTIONS: AttributeId = AttributeId::new(0x000F);
    /// `OnLevel`, writable, nullable.
    pub const ON_LEVEL: AttributeId = AttributeId::new(0x0011);
    /// `StartUpCurrentLevel`, writable, nullable.
    pub const START_UP_CURRENT_LEVEL: AttributeId = AttributeId::new(0x4000);
    /// `Options` bit `ExecuteIfOff`.
    pub const OPTION_EXECUTE_IF_OFF: u8 = 1 << 0;
    /// The lowest level a lighting device type allows.
    pub const LIGHTING_MIN_LEVEL: u8 = 1;
    /// The highest level a lighting device type allows.
    pub const LIGHTING_MAX_LEVEL: u8 = 254;
    /// `StartUpCurrentLevel` value that means "the minimum level".
    pub const START_UP_LEVEL_MINIMUM: u8 = 0;
    /// `StartUpCurrentLevel` value that means "the previous level".
    pub const START_UP_LEVEL_PREVIOUS: u8 = 255;
    /// `MoveToLevel` command.
    pub const MOVE_TO_LEVEL: CommandId = CommandId::new(0x00);
    /// `Move` command.
    pub const MOVE: CommandId = CommandId::new(0x01);
    /// `Step` command.
    pub const STEP: CommandId = CommandId::new(0x02);
    /// `Stop` command.
    pub const STOP: CommandId = CommandId::new(0x03);
    /// `MoveToLevelWithOnOff` command.
    pub const MOVE_TO_LEVEL_WITH_ON_OFF: CommandId = CommandId::new(0x04);
    /// `MoveWithOnOff` command.
    pub const MOVE_WITH_ON_OFF: CommandId = CommandId::new(0x05);
    /// `StepWithOnOff` command.
    pub const STEP_WITH_ON_OFF: CommandId = CommandId::new(0x06);
    /// `StopWithOnOff` command.
    pub const STOP_WITH_ON_OFF: CommandId = CommandId::new(0x07);
    /// `MoveMode` and `StepMode` value `Up`.
    pub const MODE_UP: u8 = 0;
    /// `MoveMode` and `StepMode` value `Down`.
    pub const MODE_DOWN: u8 = 1;
}

/// Color Control cluster (`clusters/ColorControl.xml`, revision 9).
pub mod color_control {
    use super::{AttributeId, ClusterId, CommandId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0300);
    /// The cluster revision.
    pub const REVISION: u16 = 9;
    /// Feature `HueSaturation` (bit 0).
    pub const FEATURE_HUE_SATURATION: u32 = 1 << 0;
    /// Feature `XY` (bit 3).
    pub const FEATURE_XY: u32 = 1 << 3;
    /// Feature `ColorTemperature` (bit 4).
    pub const FEATURE_COLOR_TEMPERATURE: u32 = 1 << 4;
    /// `CurrentHue`.
    pub const CURRENT_HUE: AttributeId = AttributeId::new(0x0000);
    /// `CurrentSaturation`.
    pub const CURRENT_SATURATION: AttributeId = AttributeId::new(0x0001);
    /// `RemainingTime`, tenths of a second.
    pub const REMAINING_TIME: AttributeId = AttributeId::new(0x0002);
    /// `CurrentX`.
    pub const CURRENT_X: AttributeId = AttributeId::new(0x0003);
    /// `CurrentY`.
    pub const CURRENT_Y: AttributeId = AttributeId::new(0x0004);
    /// `ColorTemperatureMireds`.
    pub const COLOR_TEMPERATURE_MIREDS: AttributeId = AttributeId::new(0x0007);
    /// `ColorMode`.
    pub const COLOR_MODE: AttributeId = AttributeId::new(0x0008);
    /// `Options`, writable bitmap.
    pub const OPTIONS: AttributeId = AttributeId::new(0x000F);
    /// `NumberOfPrimaries`, nullable.
    pub const NUMBER_OF_PRIMARIES: AttributeId = AttributeId::new(0x0010);
    /// `EnhancedColorMode`.
    pub const ENHANCED_COLOR_MODE: AttributeId = AttributeId::new(0x4001);
    /// `ColorCapabilities`.
    pub const COLOR_CAPABILITIES: AttributeId = AttributeId::new(0x400A);
    /// `ColorTempPhysicalMinMireds`.
    pub const COLOR_TEMP_PHYSICAL_MIN_MIREDS: AttributeId = AttributeId::new(0x400B);
    /// `ColorTempPhysicalMaxMireds`.
    pub const COLOR_TEMP_PHYSICAL_MAX_MIREDS: AttributeId = AttributeId::new(0x400C);
    /// `CoupleColorTempToLevelMinMireds`.
    pub const COUPLE_COLOR_TEMP_TO_LEVEL_MIN_MIREDS: AttributeId = AttributeId::new(0x400D);
    /// `StartUpColorTemperatureMireds`, writable, nullable.
    pub const START_UP_COLOR_TEMPERATURE_MIREDS: AttributeId = AttributeId::new(0x4010);
    /// `Options` bit `ExecuteIfOff`.
    pub const OPTION_EXECUTE_IF_OFF: u8 = 1 << 0;
    /// `ColorMode` and `EnhancedColorMode` value `CurrentHueAndCurrentSaturation`.
    pub const MODE_HUE_SATURATION: u8 = 0;
    /// `ColorMode` and `EnhancedColorMode` value `CurrentXAndCurrentY`.
    pub const MODE_XY: u8 = 1;
    /// `ColorMode` and `EnhancedColorMode` value `ColorTemperatureMireds`.
    pub const MODE_COLOR_TEMPERATURE: u8 = 2;
    /// The largest hue and saturation value.
    pub const MAX_HUE_SATURATION: u8 = 254;
    /// The largest `CurrentX`, `CurrentY` and mireds value.
    pub const MAX_COLOR_COORDINATE: u16 = 0xFEFF;
    /// The smallest mireds value `StartUpColorTemperatureMireds` accepts.
    pub const MIN_MIREDS: u16 = 1;
    /// The specification's default `CurrentX` (a warm white).
    pub const DEFAULT_CURRENT_X: u16 = 0x616B;
    /// The specification's default `CurrentY`.
    pub const DEFAULT_CURRENT_Y: u16 = 0x607D;
    /// `MoveToHue` command.
    pub const MOVE_TO_HUE: CommandId = CommandId::new(0x00);
    /// `MoveHue` command.
    pub const MOVE_HUE: CommandId = CommandId::new(0x01);
    /// `StepHue` command.
    pub const STEP_HUE: CommandId = CommandId::new(0x02);
    /// `MoveToSaturation` command.
    pub const MOVE_TO_SATURATION: CommandId = CommandId::new(0x03);
    /// `MoveSaturation` command.
    pub const MOVE_SATURATION: CommandId = CommandId::new(0x04);
    /// `StepSaturation` command.
    pub const STEP_SATURATION: CommandId = CommandId::new(0x05);
    /// `MoveToHueAndSaturation` command.
    pub const MOVE_TO_HUE_AND_SATURATION: CommandId = CommandId::new(0x06);
    /// `MoveToColor` command.
    pub const MOVE_TO_COLOR: CommandId = CommandId::new(0x07);
    /// `MoveColor` command.
    pub const MOVE_COLOR: CommandId = CommandId::new(0x08);
    /// `StepColor` command.
    pub const STEP_COLOR: CommandId = CommandId::new(0x09);
    /// `MoveToColorTemperature` command.
    pub const MOVE_TO_COLOR_TEMPERATURE: CommandId = CommandId::new(0x0A);
    /// `StopMoveStep` command.
    pub const STOP_MOVE_STEP: CommandId = CommandId::new(0x47);
    /// `MoveColorTemperature` command.
    pub const MOVE_COLOR_TEMPERATURE: CommandId = CommandId::new(0x4B);
    /// `StepColorTemperature` command.
    pub const STEP_COLOR_TEMPERATURE: CommandId = CommandId::new(0x4C);
    /// `MoveMode` value `Stop`.
    pub const MOVE_STOP: u8 = 0;
    /// `MoveMode` and `StepMode` value `Up`.
    pub const MODE_UP: u8 = 1;
    /// `MoveMode` and `StepMode` value `Down`.
    pub const MODE_DOWN: u8 = 3;
}

/// Boolean State cluster (`clusters/BooleanState.xml`, revision 3).
pub mod boolean_state {
    use super::{AttributeId, ClusterId, EventId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0045);
    /// The cluster revision.
    pub const REVISION: u16 = 3;
    /// `StateValue`.
    pub const STATE_VALUE: AttributeId = AttributeId::new(0x0000);
    /// `StateChange` event; its one field (tag 0) is the new `StateValue`.
    pub const STATE_CHANGE: EventId = EventId::new(0x00);
}

/// Temperature Measurement cluster (`clusters/TemperatureMeasurement.xml`, revision 6).
pub mod temperature_measurement {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0402);
    /// The cluster revision.
    pub const REVISION: u16 = 6;
    /// `MeasuredValue` in hundredths of a degree Celsius, nullable.
    pub const MEASURED_VALUE: AttributeId = AttributeId::new(0x0000);
    /// `MinMeasuredValue`, nullable.
    pub const MIN_MEASURED_VALUE: AttributeId = AttributeId::new(0x0001);
    /// `MaxMeasuredValue`, nullable.
    pub const MAX_MEASURED_VALUE: AttributeId = AttributeId::new(0x0002);
}

/// Relative Humidity Measurement cluster (`clusters/WaterContentMeasurement.xml`, revision 5).
pub mod relative_humidity_measurement {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0405);
    /// The cluster revision.
    pub const REVISION: u16 = 5;
    /// `MeasuredValue` in hundredths of a percent, nullable.
    pub const MEASURED_VALUE: AttributeId = AttributeId::new(0x0000);
    /// `MinMeasuredValue`, nullable.
    pub const MIN_MEASURED_VALUE: AttributeId = AttributeId::new(0x0001);
    /// `MaxMeasuredValue`, nullable.
    pub const MAX_MEASURED_VALUE: AttributeId = AttributeId::new(0x0002);
}

/// Pressure Measurement cluster (`clusters/PressureMeasurement.xml`, revision 5).
pub mod pressure_measurement {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0403);
    /// The cluster revision.
    pub const REVISION: u16 = 5;
    /// `MeasuredValue` in kilopascal times ten, nullable.
    pub const MEASURED_VALUE: AttributeId = AttributeId::new(0x0000);
    /// `MinMeasuredValue`, nullable.
    pub const MIN_MEASURED_VALUE: AttributeId = AttributeId::new(0x0001);
    /// `MaxMeasuredValue`, nullable.
    pub const MAX_MEASURED_VALUE: AttributeId = AttributeId::new(0x0002);
}

/// Illuminance Measurement cluster (`clusters/IlluminanceMeasurement.xml`, revision 5).
pub mod illuminance_measurement {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0400);
    /// The cluster revision.
    pub const REVISION: u16 = 5;
    /// `MeasuredValue`, `10000 * log10(lux) + 1`, nullable.
    pub const MEASURED_VALUE: AttributeId = AttributeId::new(0x0000);
    /// `MinMeasuredValue`, nullable.
    pub const MIN_MEASURED_VALUE: AttributeId = AttributeId::new(0x0001);
    /// `MaxMeasuredValue`, nullable.
    pub const MAX_MEASURED_VALUE: AttributeId = AttributeId::new(0x0002);
}

/// Occupancy Sensing cluster (`clusters/OccupancySensing.xml`, revision 7).
pub mod occupancy_sensing {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x0406);
    /// The cluster revision.
    pub const REVISION: u16 = 7;
    /// Feature `Other` (bit 0): a sensing modality the other feature bits do not name.
    pub const FEATURE_OTHER: u32 = 1 << 0;
    /// `Occupancy` bitmap; bit 0 is `Occupied`.
    pub const OCCUPANCY: AttributeId = AttributeId::new(0x0000);
    /// `OccupancySensorType`.
    pub const OCCUPANCY_SENSOR_TYPE: AttributeId = AttributeId::new(0x0001);
    /// `OccupancySensorTypeBitmap`.
    pub const OCCUPANCY_SENSOR_TYPE_BITMAP: AttributeId = AttributeId::new(0x0002);
    /// `Occupancy` bit `Occupied`.
    pub const OCCUPIED: u8 = 1 << 0;
    /// `OccupancySensorType` value `PIR`, which the specification prescribes for sensors whose
    /// modality has no value of its own.
    pub const SENSOR_TYPE_PIR: u8 = 0;
    /// `OccupancySensorTypeBitmap` bit `PIR`.
    pub const SENSOR_TYPE_BITMAP_PIR: u8 = 1 << 0;
}

/// Switch cluster (`clusters/Switch.xml`, revision 2).
pub mod switch {
    use super::{AttributeId, ClusterId, EventId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x003B);
    /// The cluster revision.
    pub const REVISION: u16 = 2;
    /// Feature `MomentarySwitch` (bit 1).
    pub const FEATURE_MOMENTARY: u32 = 1 << 1;
    /// Feature `MomentarySwitchRelease` (bit 2).
    pub const FEATURE_MOMENTARY_RELEASE: u32 = 1 << 2;
    /// Feature `MomentarySwitchLongPress` (bit 3).
    pub const FEATURE_MOMENTARY_LONG_PRESS: u32 = 1 << 3;
    /// Feature `MomentarySwitchMultiPress` (bit 4).
    pub const FEATURE_MOMENTARY_MULTI_PRESS: u32 = 1 << 4;
    /// `NumberOfPositions`.
    pub const NUMBER_OF_POSITIONS: AttributeId = AttributeId::new(0x0000);
    /// `CurrentPosition`.
    pub const CURRENT_POSITION: AttributeId = AttributeId::new(0x0001);
    /// `MultiPressMax`.
    pub const MULTI_PRESS_MAX: AttributeId = AttributeId::new(0x0002);
    /// `InitialPress` event (`NewPosition`).
    pub const INITIAL_PRESS: EventId = EventId::new(0x01);
    /// `LongPress` event (`NewPosition`).
    pub const LONG_PRESS: EventId = EventId::new(0x02);
    /// `ShortRelease` event (`PreviousPosition`).
    pub const SHORT_RELEASE: EventId = EventId::new(0x03);
    /// `LongRelease` event (`PreviousPosition`).
    pub const LONG_RELEASE: EventId = EventId::new(0x04);
    /// `MultiPressOngoing` event (`NewPosition`, `CurrentNumberOfPressesCounted`).
    pub const MULTI_PRESS_ONGOING: EventId = EventId::new(0x05);
    /// `MultiPressComplete` event (`PreviousPosition`, `TotalNumberOfPressesCounted`).
    pub const MULTI_PRESS_COMPLETE: EventId = EventId::new(0x06);
    /// The idle position of a momentary switch.
    pub const IDLE_POSITION: u8 = 0;
}

/// Bridged Device Basic Information cluster (`clusters/BridgedDeviceBasicInformationCluster.xml`,
/// revision 6). The ids the facade already knows (`Reachable`, `ReachableChanged`) are taken
/// from it.
pub mod bridged_device_basic_information {
    use super::{AttributeId, ClusterId};

    /// The cluster id.
    pub const CLUSTER: ClusterId = matter_rs_engine::bridged_device::CLUSTER;
    /// The cluster revision.
    pub const REVISION: u16 = 6;
    /// `NodeLabel`, writable, at most 32 characters.
    pub const NODE_LABEL: AttributeId = AttributeId::new(0x0005);
    /// `Reachable`.
    pub const REACHABLE: AttributeId = matter_rs_engine::bridged_device::REACHABLE;
    /// `UniqueID`, mandatory from revision 4, at most 32 characters.
    pub const UNIQUE_ID: AttributeId = AttributeId::new(0x0012);
    /// The longest `NodeLabel` and `UniqueID` the cluster allows.
    pub const MAX_LABEL_LENGTH: usize = 32;
}

/// Descriptor cluster (`clusters/Descriptor-Cluster.xml`, revision 3). The engine serves the
/// cluster; the bridge only computes the semantic tags of `TagList`.
pub mod descriptor {
    use super::ClusterId;

    /// The cluster id.
    pub const CLUSTER: ClusterId = ClusterId::new(0x001D);
    /// The `Common Number` semantic tag namespace (`namespaces/Namespace-Common-Number.xml`):
    /// tags `Zero` (0) to `Thirty` (30).
    pub const NAMESPACE_COMMON_NUMBER: u8 = 0x07;
    /// The highest tag of the `Common Number` namespace.
    pub const MAX_NUMBER_TAG: u8 = 30;
    /// Tag of the `MfgCode` field of `SemanticTagStruct`.
    pub const TAG_FIELD_MANUFACTURER_CODE: u64 = 0;
    /// Tag of the `NamespaceID` field of `SemanticTagStruct`.
    pub const TAG_FIELD_NAMESPACE: u64 = 1;
    /// Tag of the `Tag` field of `SemanticTagStruct`.
    pub const TAG_FIELD_TAG: u64 = 2;
    /// Tag of the `Label` field of `SemanticTagStruct`.
    pub const TAG_FIELD_LABEL: u64 = 3;
}
