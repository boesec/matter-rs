//! What the bridge hands to adapters: typed device commands and bridge events.

use alloc::string::String;
use core::time::Duration;

use matter_rs_engine::{
    ClusterId, CommandId, CommissioningState, FabricIndex, FabricInfo, StopReason, Value,
};

use crate::bridge::ResponseHandle;
use crate::model::{Attribute, DeviceKey, PartIndex};

/// A command for a device, translated from a Matter command or write.
///
/// The typed variants carry *target values*: the bridge has already set the resulting attribute
/// values on the endpoint and answered the Matter client, and tells the adapter what the device
/// should now do. A `transition` is the time the client asked the change to take; it is a hint
/// for the device, the bridge does not animate anything, and `None` means the device's default.
///
/// [`Raw`](Self::Raw) and [`Write`](Self::Write) are the escapes for the extra clusters of a
/// part: the Matter client is still waiting, and the adapter answers through the
/// [`ResponseHandle`] (or lets the bridge answer `Failure` by dropping it or by missing the
/// response timeout).
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum DeviceCommand {
    /// Switch on or off.
    SetOnOff {
        /// The target state.
        on: bool,
    },
    /// Move to a brightness level.
    SetLevel {
        /// The target level, within the device's `MinLevel..=MaxLevel`.
        level: u8,
        /// How long the change should take.
        transition: Option<Duration>,
        /// Whether the On/Off state changes with the level (`MoveToLevelWithOnOff` and friends,
        /// or an `On` command with an `OnLevel`).
        on_off: Option<bool>,
    },
    /// Move to a colour temperature.
    SetColorTemperature {
        /// The target colour temperature in mireds.
        mireds: u16,
        /// How long the change should take.
        transition: Option<Duration>,
    },
    /// Move to a hue, keeping the saturation.
    SetHue {
        /// The target hue, `0..=254`.
        hue: u8,
        /// How long the change should take.
        transition: Option<Duration>,
    },
    /// Move to a saturation, keeping the hue.
    SetSaturation {
        /// The target saturation, `0..=254`.
        saturation: u8,
        /// How long the change should take.
        transition: Option<Duration>,
    },
    /// Move to a hue and a saturation.
    SetHueAndSaturation {
        /// The target hue, `0..=254`.
        hue: u8,
        /// The target saturation, `0..=254`.
        saturation: u8,
        /// How long the change should take.
        transition: Option<Duration>,
    },
    /// Move to a colour in CIE xy.
    SetColorXy {
        /// The target x, `0..=0xFEFF`.
        x: u16,
        /// The target y, `0..=0xFEFF`.
        y: u16,
        /// How long the change should take.
        transition: Option<Duration>,
    },
    /// Identify the device to the user for a time; zero stops identification.
    Identify {
        /// How long to identify.
        duration: Duration,
    },
    /// Show an identification effect.
    TriggerEffect {
        /// The `EffectIdentifier`: `0` blink, `1` breathe, `2` okay, `11` channel change,
        /// `254` finish, `255` stop.
        effect: u8,
        /// The `EffectVariant`; `0` is the default.
        variant: u8,
    },
    /// A controller renamed the device; the bridge serves and persists the new label.
    Rename(String),
    /// A controller wrote one of the device's own writable attributes (`OnLevel`,
    /// `StartUpOnOff`, ...); the bridge accepted and stored it.
    Written {
        /// The attribute.
        attribute: Attribute,
        /// The written value.
        value: Value,
    },
    /// A controller wants to write an attribute of an extra cluster; the adapter decides.
    Write {
        /// The attribute.
        attribute: Attribute,
        /// The value the controller wants to write.
        value: Value,
        /// Answer here: `Success` makes the engine store the value.
        response: ResponseHandle,
    },
    /// A controller invoked a command of an extra cluster; the adapter decides.
    Raw {
        /// The cluster.
        cluster: ClusterId,
        /// The command.
        command: CommandId,
        /// The command fields; [`Value::Null`] when there are none.
        payload: Value,
        /// Answer here.
        response: ResponseHandle,
    },
}

/// A command with the device and part it is for.
#[derive(Debug, PartialEq)]
pub struct CommandEvent {
    /// The device.
    pub key: DeviceKey,
    /// The part, or `None` for the device as a whole (a [`DeviceCommand::Rename`]).
    pub part: Option<PartIndex>,
    /// The command.
    pub command: DeviceCommand,
}

/// What the bridge reports: device commands and the node's lifecycle.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum BridgeEvent {
    /// A command for a device.
    Command(CommandEvent),
    /// The commissioning state changed.
    CommissioningChanged(CommissioningState),
    /// The node joined a fabric.
    FabricAdded(FabricInfo),
    /// The node left a fabric.
    FabricRemoved(FabricIndex),
    /// The node stopped; no further event follows.
    Stopped(StopReason),
}
