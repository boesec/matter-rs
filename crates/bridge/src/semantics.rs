//! The cluster semantics: from Matter commands and writes to target values and device commands.
//!
//! The bridge passes commands through with target values: every command that defines a resulting
//! state sets that state on the endpoint (`On` sets `OnOff`, `MoveToLevel` sets `CurrentLevel`,
//! `MoveToColorTemperature` sets `ColorTemperatureMireds` and `ColorMode`, ...) and becomes one
//! typed [`DeviceCommand`] for the adapter. Continuous commands (`Move`, `Step`, their colour
//! counterparts) are translated into the target they would reach: a step lands at the current
//! value plus or minus the step size, a move at the end of the range in its direction, with a
//! transition hint computed from the rate; `Stop` targets the current value with a zero
//! transition. Nothing is animated here; transitions are hints for the device behind the adapter.
//!
//! The functions in this module are pure: they read the endpoint's values and return what the
//! bridge should set, answer and tell the adapter.

use alloc::vec::Vec;
use core::time::Duration;

use matter_rs_engine::{AttributeId, ClusterId, CommandId, Status, Value};

use crate::command::DeviceCommand;
use crate::ids::{
    bridged_device_basic_information as bdbi, color_control as cc, identify, level_control as lc,
    on_off,
};
use crate::kinds::Access;
use crate::model::Attribute;
use crate::plan::PlannedEndpoint;

/// What a Matter command amounts to.
#[derive(Debug, PartialEq)]
pub(crate) struct Translation {
    /// The status the bridge answers the client with.
    pub status: Status,
    /// The attribute values that result; the bridge sets them on the endpoint.
    pub changes: Vec<(ClusterId, AttributeId, Value)>,
    /// The commands for the adapter.
    pub commands: Vec<DeviceCommand>,
}

impl Translation {
    fn status(status: Status) -> Self {
        Self {
            status,
            changes: Vec::new(),
            commands: Vec::new(),
        }
    }

    fn success() -> Self {
        Self::status(Status::Success)
    }

    fn change(mut self, attribute: Attribute, value: impl Into<Value>) -> Self {
        let (cluster, id) = attribute.path();
        self.changes.push((cluster, id, value.into()));
        self
    }

    fn command(mut self, command: DeviceCommand) -> Self {
        self.commands.push(command);
        self
    }
}

/// What a Matter command means for an endpoint.
#[derive(Debug, PartialEq)]
pub(crate) enum CommandOutcome {
    /// The bridge handles it as described.
    Handled(Translation),
    /// A command of an extra cluster: the adapter answers.
    Forward,
}

/// What a Matter write means for an endpoint.
#[derive(Debug, PartialEq)]
pub(crate) enum WriteOutcome {
    /// The bridge accepts the write; the engine stores the value, the bridge keeps `stored` (in
    /// the attribute's width), persists it when the attribute is persisted, and tells the
    /// adapter with `commands`.
    Accepted {
        /// The written value in the attribute's width.
        stored: Value,
        /// Whether the value survives a restart.
        persist: bool,
        /// The commands for the adapter.
        commands: Vec<DeviceCommand>,
    },
    /// The bridge refuses the write with this status.
    Rejected(Status),
    /// A write to an extra cluster: the adapter answers.
    Forward,
}

/// Translates a command invoked on an endpoint.
pub(crate) fn translate_command(
    endpoint: &PlannedEndpoint,
    cluster: ClusterId,
    command: CommandId,
    payload: &Value,
) -> CommandOutcome {
    let Some(spec) = endpoint.cluster(cluster) else {
        return CommandOutcome::Handled(Translation::status(Status::UnsupportedCluster));
    };
    if !spec.commands.contains(&command) {
        return CommandOutcome::Handled(Translation::status(Status::UnsupportedCommand));
    }
    let values = Values(endpoint);
    let translation = match cluster {
        on_off::CLUSTER => on_off_command(&values, command, payload),
        lc::CLUSTER => level_command(&values, command, payload),
        cc::CLUSTER => color_command(&values, spec.feature_map, command, payload),
        identify::CLUSTER => identify_command(command, payload),
        _ => return CommandOutcome::Forward,
    };
    CommandOutcome::Handled(translation)
}

/// Translates a write requested on an endpoint.
pub(crate) fn translate_write(
    endpoint: &PlannedEndpoint,
    cluster: ClusterId,
    attribute: AttributeId,
    value: &Value,
) -> WriteOutcome {
    let typed = Attribute::from_path(cluster, attribute);
    let Some(spec) = endpoint.attribute(typed) else {
        return WriteOutcome::Rejected(Status::UnsupportedAttribute);
    };
    if endpoint.cluster(cluster).is_none_or(|c| c.id != cluster) {
        return WriteOutcome::Rejected(Status::UnsupportedCluster);
    }
    if spec.kind.is_none() {
        return WriteOutcome::Forward;
    }
    if spec.access == Access::ReadOnly {
        return WriteOutcome::Rejected(Status::UnsupportedWrite);
    }
    let stored = match spec.check(value) {
        Ok(stored) => stored,
        Err(_) if value.is_null() || value.kind() == spec.kind.unwrap_or(value.kind()) => {
            return WriteOutcome::Rejected(Status::ConstraintError);
        }
        Err(_) => return WriteOutcome::Rejected(Status::InvalidDataType),
    };
    let mut commands = Vec::new();
    match typed {
        Attribute::Raw(c, a) if (c, a) == (bdbi::CLUSTER, bdbi::NODE_LABEL) => {
            let Value::Utf8(label) = &stored else {
                return WriteOutcome::Rejected(Status::InvalidDataType);
            };
            if label.chars().count() > bdbi::MAX_LABEL_LENGTH {
                return WriteOutcome::Rejected(Status::ConstraintError);
            }
            commands.push(DeviceCommand::Rename(label.clone()));
        }
        Attribute::IdentifyTime => {
            let seconds = u16::try_from(stored.clone()).unwrap_or(0);
            commands.push(DeviceCommand::Identify {
                duration: Duration::from_secs(u64::from(seconds)),
            });
        }
        other => commands.push(DeviceCommand::Written {
            attribute: other,
            value: stored.clone(),
        }),
    }
    WriteOutcome::Accepted {
        stored,
        persist: spec.access == Access::Persisted,
        commands,
    }
}

/// Read access to an endpoint's values.
struct Values<'a>(&'a PlannedEndpoint);

impl Values<'_> {
    fn get(&self, attribute: Attribute) -> Option<&Value> {
        self.0.values.get(&attribute.path())
    }

    fn has(&self, attribute: Attribute) -> bool {
        self.0.attribute(attribute).is_some()
    }

    fn bool(&self, attribute: Attribute) -> Option<bool> {
        self.get(attribute)
            .and_then(|v| bool::try_from(v.clone()).ok())
    }

    fn u8(&self, attribute: Attribute) -> Option<u8> {
        self.get(attribute)
            .and_then(|v| u8::try_from(v.clone()).ok())
    }

    fn u16(&self, attribute: Attribute) -> Option<u16> {
        self.get(attribute)
            .and_then(|v| u16::try_from(v.clone()).ok())
    }

    fn is_on(&self) -> bool {
        self.bool(Attribute::OnOff).unwrap_or(true)
    }

    fn min_level(&self) -> u8 {
        self.u8(Attribute::Raw(lc::CLUSTER, lc::MIN_LEVEL))
            .unwrap_or(lc::LIGHTING_MIN_LEVEL)
    }

    fn max_level(&self) -> u8 {
        self.u8(Attribute::Raw(lc::CLUSTER, lc::MAX_LEVEL))
            .unwrap_or(lc::LIGHTING_MAX_LEVEL)
    }

    /// Whether a command of a cluster with an `ExecuteIfOff` option runs while the device is
    /// off, given the stored options and the command's `OptionsMask` and `OptionsOverride`.
    fn executes(&self, options: Attribute, mask: Option<u8>, override_: Option<u8>) -> bool {
        if self.is_on() {
            return true;
        }
        let stored = self.u8(options).unwrap_or(0);
        let mask = mask.unwrap_or(0);
        let effective = (stored & !mask) | (override_.unwrap_or(0) & mask);
        effective & lc::OPTION_EXECUTE_IF_OFF != 0
    }
}

/// The field with `tag` of a struct payload.
fn field(payload: &Value, tag: u64) -> Option<&Value> {
    match payload {
        Value::Struct(fields) => fields.iter().find(|(t, _)| *t == tag).map(|(_, v)| v),
        _ => None,
    }
}

fn u8_field(payload: &Value, tag: u64) -> Option<u8> {
    field(payload, tag).and_then(|v| u8::try_from(v.clone()).ok())
}

fn u16_field(payload: &Value, tag: u64) -> Option<u16> {
    field(payload, tag).and_then(|v| u16::try_from(v.clone()).ok())
}

fn i16_field(payload: &Value, tag: u64) -> Option<i16> {
    field(payload, tag).and_then(|v| i16::try_from(v.clone()).ok())
}

/// A nullable field: `None` when missing or malformed, `Some(None)` when null.
fn nullable_u16_field(payload: &Value, tag: u64) -> Option<Option<u16>> {
    field(payload, tag).and_then(|v| Option::<u16>::try_from(v.clone()).ok())
}

fn nullable_u8_field(payload: &Value, tag: u64) -> Option<Option<u8>> {
    field(payload, tag).and_then(|v| Option::<u8>::try_from(v.clone()).ok())
}

/// A transition time in tenths of a second as a duration.
fn tenths(value: u16) -> Duration {
    Duration::from_millis(u64::from(value) * 100)
}

/// The time a move over `distance` units takes at `rate` units per second; `None` without a
/// usable rate.
fn travel(distance: u32, rate: Option<u32>) -> Option<Duration> {
    match rate {
        Some(rate) if rate > 0 => Some(Duration::from_millis(
            u64::from(distance) * 1000 / u64::from(rate),
        )),
        _ => None,
    }
}

fn on_off_command(values: &Values<'_>, command: CommandId, payload: &Value) -> Translation {
    let current = values.is_on();
    let (on, global_scene) = match command {
        on_off::OFF => (false, None),
        on_off::ON | on_off::ON_WITH_RECALL_GLOBAL_SCENE => (true, Some(true)),
        on_off::TOGGLE => (!current, if current { None } else { Some(true) }),
        on_off::OFF_WITH_EFFECT => {
            if u8_field(payload, 0).is_none() || u8_field(payload, 1).is_none() {
                return Translation::status(Status::InvalidCommand);
            }
            (false, Some(false))
        }
        on_off::ON_WITH_TIMED_OFF => {
            let (Some(_control), Some(on_time), Some(off_wait)) = (
                u8_field(payload, 0),
                u16_field(payload, 1),
                u16_field(payload, 2),
            ) else {
                return Translation::status(Status::InvalidCommand);
            };
            return turn(values, true, Some(true))
                .change(Attribute::OnTime, on_time)
                .change(Attribute::OffWaitTime, off_wait);
        }
        _ => return Translation::status(Status::UnsupportedCommand),
    };
    turn(values, on, global_scene)
}

/// Switches on or off. Turning on a device with a Level Control cluster whose `OnLevel` is set
/// also moves it to that level, as the cluster's `OnOff` feature prescribes.
fn turn(values: &Values<'_>, on: bool, global_scene: Option<bool>) -> Translation {
    let mut translation = Translation::success().change(Attribute::OnOff, on);
    if let Some(global_scene) = global_scene
        && values.has(Attribute::Raw(
            on_off::CLUSTER,
            on_off::GLOBAL_SCENE_CONTROL,
        ))
    {
        translation = translation.change(
            Attribute::Raw(on_off::CLUSTER, on_off::GLOBAL_SCENE_CONTROL),
            global_scene,
        );
    }
    let on_level = if on {
        values.u8(Attribute::OnLevel)
    } else {
        None
    };
    match on_level {
        Some(level) => {
            translation
                .change(Attribute::CurrentLevel, level)
                .command(DeviceCommand::SetLevel {
                    level,
                    transition: None,
                    on_off: Some(true),
                })
        }
        None => translation.command(DeviceCommand::SetOnOff { on }),
    }
}

fn level_command(values: &Values<'_>, command: CommandId, payload: &Value) -> Translation {
    let min = values.min_level();
    let max = values.max_level();
    let with_on_off = matches!(
        command,
        lc::MOVE_TO_LEVEL_WITH_ON_OFF
            | lc::MOVE_WITH_ON_OFF
            | lc::STEP_WITH_ON_OFF
            | lc::STOP_WITH_ON_OFF
    );
    let (target, transition, options): (u8, Option<Duration>, (Option<u8>, Option<u8>)) =
        match command {
            lc::MOVE_TO_LEVEL | lc::MOVE_TO_LEVEL_WITH_ON_OFF => {
                let (Some(level), Some(transition)) =
                    (u8_field(payload, 0), nullable_u16_field(payload, 1))
                else {
                    return Translation::status(Status::InvalidCommand);
                };
                (
                    level.clamp(min, max),
                    transition.map(tenths),
                    (u8_field(payload, 2), u8_field(payload, 3)),
                )
            }
            lc::MOVE | lc::MOVE_WITH_ON_OFF => {
                let (Some(mode), Some(rate)) =
                    (u8_field(payload, 0), nullable_u8_field(payload, 1))
                else {
                    return Translation::status(Status::InvalidCommand);
                };
                let Some(current) = values.u8(Attribute::CurrentLevel) else {
                    return Translation::status(Status::InvalidInState);
                };
                let target = match mode {
                    lc::MODE_UP => max,
                    lc::MODE_DOWN => min,
                    _ => return Translation::status(Status::InvalidCommand),
                };
                let distance = u32::from(current.abs_diff(target));
                (
                    target,
                    travel(distance, rate.map(u32::from)),
                    (u8_field(payload, 2), u8_field(payload, 3)),
                )
            }
            lc::STEP | lc::STEP_WITH_ON_OFF => {
                let (Some(mode), Some(step), Some(transition)) = (
                    u8_field(payload, 0),
                    u8_field(payload, 1),
                    nullable_u16_field(payload, 2),
                ) else {
                    return Translation::status(Status::InvalidCommand);
                };
                let Some(current) = values.u8(Attribute::CurrentLevel) else {
                    return Translation::status(Status::InvalidInState);
                };
                let target = match mode {
                    lc::MODE_UP => current.saturating_add(step).min(max),
                    lc::MODE_DOWN => current.saturating_sub(step).max(min),
                    _ => return Translation::status(Status::InvalidCommand),
                };
                (
                    target,
                    transition.map(tenths),
                    (u8_field(payload, 3), u8_field(payload, 4)),
                )
            }
            lc::STOP | lc::STOP_WITH_ON_OFF => {
                let Some(current) = values.u8(Attribute::CurrentLevel) else {
                    return Translation::success();
                };
                (
                    current,
                    Some(Duration::ZERO),
                    (u8_field(payload, 0), u8_field(payload, 1)),
                )
            }
            _ => return Translation::status(Status::UnsupportedCommand),
        };
    if !with_on_off && !values.executes(Attribute::LevelOptions, options.0, options.1) {
        return Translation::success();
    }
    let mut translation = Translation::success().change(Attribute::CurrentLevel, target);
    let on_off = if with_on_off {
        let on = target > min;
        translation = translation.change(Attribute::OnOff, on);
        Some(on)
    } else {
        None
    };
    translation.command(DeviceCommand::SetLevel {
        level: target,
        transition,
        on_off,
    })
}

/// The target of a continuous move within `min..=max`: the end of the range in the move's
/// direction, or the current value for `Stop`. Returns the target and the transition.
fn move_target(
    mode: u8,
    rate: u16,
    current: u16,
    min: u16,
    max: u16,
) -> Option<(u16, Option<Duration>)> {
    let target = match mode {
        cc::MOVE_STOP => return Some((current, Some(Duration::ZERO))),
        cc::MODE_UP => max.max(min),
        cc::MODE_DOWN => min.min(max),
        _ => return None,
    };
    let distance = u32::from(current.abs_diff(target));
    Some((target, travel(distance, Some(u32::from(rate)))))
}

/// The target of a step within `min..=max`.
fn step_target(mode: u8, step: u16, current: u16, min: u16, max: u16) -> Option<u16> {
    match mode {
        cc::MODE_UP => Some(current.saturating_add(step).min(max).max(min)),
        cc::MODE_DOWN => Some(current.saturating_sub(step).max(min).min(max)),
        _ => None,
    }
}

fn color_command(
    values: &Values<'_>,
    features: u32,
    command: CommandId,
    payload: &Value,
) -> Translation {
    let hue = u16::from(values.u8(Attribute::CurrentHue).unwrap_or(0));
    let saturation = u16::from(values.u8(Attribute::CurrentSaturation).unwrap_or(0));
    let x = values
        .u16(Attribute::CurrentX)
        .unwrap_or(cc::DEFAULT_CURRENT_X);
    let y = values
        .u16(Attribute::CurrentY)
        .unwrap_or(cc::DEFAULT_CURRENT_Y);
    let mireds = values.u16(Attribute::ColorTemperatureMireds).unwrap_or(0);
    let physical_min = values
        .u16(Attribute::ColorTempPhysicalMinMireds)
        .unwrap_or(0);
    let physical_max = values
        .u16(Attribute::ColorTempPhysicalMaxMireds)
        .unwrap_or(cc::MAX_COLOR_COORDINATE);
    let max_hs = u16::from(cc::MAX_HUE_SATURATION);
    let max_xy = cc::MAX_COLOR_COORDINATE;
    let invalid = || Translation::status(Status::InvalidCommand);

    /// A colour target: what to set and what to tell the adapter.
    enum Target {
        Hue(u16, Option<Duration>),
        Saturation(u16, Option<Duration>),
        HueSaturation(u16, u16, Option<Duration>),
        Xy(u16, u16, Option<Duration>),
        Mireds(u16, Option<Duration>),
    }

    let (target, options) = match command {
        cc::MOVE_TO_HUE => {
            let (Some(hue), Some(_direction), Some(transition)) = (
                u8_field(payload, 0),
                u8_field(payload, 1),
                u16_field(payload, 2),
            ) else {
                return invalid();
            };
            (
                Target::Hue(u16::from(hue).min(max_hs), Some(tenths(transition))),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_HUE => {
            let (Some(mode), Some(rate)) = (u8_field(payload, 0), u8_field(payload, 1)) else {
                return invalid();
            };
            let Some((target, transition)) = move_target(mode, u16::from(rate), hue, 0, max_hs)
            else {
                return invalid();
            };
            (
                Target::Hue(target, transition),
                (u8_field(payload, 2), u8_field(payload, 3)),
            )
        }
        cc::STEP_HUE => {
            let (Some(mode), Some(step), Some(transition)) = (
                u8_field(payload, 0),
                u8_field(payload, 1),
                u8_field(payload, 2),
            ) else {
                return invalid();
            };
            // Hue wraps around the colour circle.
            let step = i32::from(step);
            let target = match mode {
                cc::MODE_UP => (i32::from(hue) + step).rem_euclid(i32::from(max_hs) + 1),
                cc::MODE_DOWN => (i32::from(hue) - step).rem_euclid(i32::from(max_hs) + 1),
                _ => return invalid(),
            };
            (
                Target::Hue(
                    u16::try_from(target).unwrap_or(0),
                    Some(tenths(u16::from(transition))),
                ),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_TO_SATURATION => {
            let (Some(saturation), Some(transition)) =
                (u8_field(payload, 0), u16_field(payload, 1))
            else {
                return invalid();
            };
            (
                Target::Saturation(u16::from(saturation).min(max_hs), Some(tenths(transition))),
                (u8_field(payload, 2), u8_field(payload, 3)),
            )
        }
        cc::MOVE_SATURATION => {
            let (Some(mode), Some(rate)) = (u8_field(payload, 0), u8_field(payload, 1)) else {
                return invalid();
            };
            let Some((target, transition)) =
                move_target(mode, u16::from(rate), saturation, 0, max_hs)
            else {
                return invalid();
            };
            (
                Target::Saturation(target, transition),
                (u8_field(payload, 2), u8_field(payload, 3)),
            )
        }
        cc::STEP_SATURATION => {
            let (Some(mode), Some(step), Some(transition)) = (
                u8_field(payload, 0),
                u8_field(payload, 1),
                u8_field(payload, 2),
            ) else {
                return invalid();
            };
            let Some(target) = step_target(mode, u16::from(step), saturation, 0, max_hs) else {
                return invalid();
            };
            (
                Target::Saturation(target, Some(tenths(u16::from(transition)))),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_TO_HUE_AND_SATURATION => {
            let (Some(hue), Some(saturation), Some(transition)) = (
                u8_field(payload, 0),
                u8_field(payload, 1),
                u16_field(payload, 2),
            ) else {
                return invalid();
            };
            (
                Target::HueSaturation(
                    u16::from(hue).min(max_hs),
                    u16::from(saturation).min(max_hs),
                    Some(tenths(transition)),
                ),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_TO_COLOR => {
            let (Some(x), Some(y), Some(transition)) = (
                u16_field(payload, 0),
                u16_field(payload, 1),
                u16_field(payload, 2),
            ) else {
                return invalid();
            };
            (
                Target::Xy(x.min(max_xy), y.min(max_xy), Some(tenths(transition))),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_COLOR => {
            let (Some(rate_x), Some(rate_y)) = (i16_field(payload, 0), i16_field(payload, 1))
            else {
                return invalid();
            };
            let axis = |rate: i16, current: u16| -> (u16, Option<Duration>) {
                let target = match rate {
                    0 => current,
                    r if r > 0 => max_xy,
                    _ => 0,
                };
                let distance = u32::from(current.abs_diff(target));
                (
                    target,
                    travel(distance, Some(u32::from(rate.unsigned_abs()))),
                )
            };
            let (target_x, time_x) = axis(rate_x, x);
            let (target_y, time_y) = axis(rate_y, y);
            let transition = match (time_x, time_y) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b).or(Some(Duration::ZERO)),
            };
            (
                Target::Xy(target_x, target_y, transition),
                (u8_field(payload, 2), u8_field(payload, 3)),
            )
        }
        cc::STEP_COLOR => {
            let (Some(step_x), Some(step_y), Some(transition)) = (
                i16_field(payload, 0),
                i16_field(payload, 1),
                u16_field(payload, 2),
            ) else {
                return invalid();
            };
            let stepped = |current: u16, step: i16| {
                u16::try_from((i32::from(current) + i32::from(step)).clamp(0, i32::from(max_xy)))
                    .unwrap_or(0)
            };
            (
                Target::Xy(
                    stepped(x, step_x),
                    stepped(y, step_y),
                    Some(tenths(transition)),
                ),
                (u8_field(payload, 3), u8_field(payload, 4)),
            )
        }
        cc::MOVE_TO_COLOR_TEMPERATURE => {
            let (Some(target), Some(transition)) = (u16_field(payload, 0), u16_field(payload, 1))
            else {
                return invalid();
            };
            (
                Target::Mireds(
                    target.clamp(physical_min, physical_max.max(physical_min)),
                    Some(tenths(transition)),
                ),
                (u8_field(payload, 2), u8_field(payload, 3)),
            )
        }
        cc::MOVE_COLOR_TEMPERATURE => {
            let (Some(mode), Some(rate), Some(min), Some(max)) = (
                u8_field(payload, 0),
                u16_field(payload, 1),
                u16_field(payload, 2),
                u16_field(payload, 3),
            ) else {
                return invalid();
            };
            let (min, max) = temperature_limits(min, max, physical_min, physical_max);
            let Some((target, transition)) = move_target(mode, rate, mireds, min, max) else {
                return invalid();
            };
            (
                Target::Mireds(target, transition),
                (u8_field(payload, 4), u8_field(payload, 5)),
            )
        }
        cc::STEP_COLOR_TEMPERATURE => {
            let (Some(mode), Some(step), Some(transition), Some(min), Some(max)) = (
                u8_field(payload, 0),
                u16_field(payload, 1),
                u16_field(payload, 2),
                u16_field(payload, 3),
                u16_field(payload, 4),
            ) else {
                return invalid();
            };
            let (min, max) = temperature_limits(min, max, physical_min, physical_max);
            let Some(target) = step_target(mode, step, mireds, min, max) else {
                return invalid();
            };
            (
                Target::Mireds(target, Some(tenths(transition))),
                (u8_field(payload, 5), u8_field(payload, 6)),
            )
        }
        cc::STOP_MOVE_STEP => {
            let mode = values
                .u8(Attribute::ColorMode)
                .unwrap_or(cc::MODE_COLOR_TEMPERATURE);
            let target = match mode {
                cc::MODE_HUE_SATURATION if features & cc::FEATURE_HUE_SATURATION != 0 => {
                    Target::HueSaturation(hue, saturation, Some(Duration::ZERO))
                }
                cc::MODE_XY if features & cc::FEATURE_XY != 0 => {
                    Target::Xy(x, y, Some(Duration::ZERO))
                }
                _ => Target::Mireds(mireds, Some(Duration::ZERO)),
            };
            (target, (u8_field(payload, 0), u8_field(payload, 1)))
        }
        _ => return Translation::status(Status::UnsupportedCommand),
    };
    if !values.executes(Attribute::ColorOptions, options.0, options.1) {
        return Translation::success();
    }
    let narrow = |v: u16| u8::try_from(v).unwrap_or(cc::MAX_HUE_SATURATION);
    let mode = |translation: Translation, mode: u8| {
        translation
            .change(Attribute::ColorMode, mode)
            .change(Attribute::Raw(cc::CLUSTER, cc::ENHANCED_COLOR_MODE), mode)
    };
    match target {
        Target::Hue(target, transition) => mode(
            Translation::success().change(Attribute::CurrentHue, narrow(target)),
            cc::MODE_HUE_SATURATION,
        )
        .command(DeviceCommand::SetHue {
            hue: narrow(target),
            transition,
        }),
        Target::Saturation(target, transition) => mode(
            Translation::success().change(Attribute::CurrentSaturation, narrow(target)),
            cc::MODE_HUE_SATURATION,
        )
        .command(DeviceCommand::SetSaturation {
            saturation: narrow(target),
            transition,
        }),
        Target::HueSaturation(hue, saturation, transition) => mode(
            Translation::success()
                .change(Attribute::CurrentHue, narrow(hue))
                .change(Attribute::CurrentSaturation, narrow(saturation)),
            cc::MODE_HUE_SATURATION,
        )
        .command(DeviceCommand::SetHueAndSaturation {
            hue: narrow(hue),
            saturation: narrow(saturation),
            transition,
        }),
        Target::Xy(x, y, transition) => mode(
            Translation::success()
                .change(Attribute::CurrentX, x)
                .change(Attribute::CurrentY, y),
            cc::MODE_XY,
        )
        .command(DeviceCommand::SetColorXy { x, y, transition }),
        Target::Mireds(target, transition) => mode(
            Translation::success().change(Attribute::ColorTemperatureMireds, target),
            cc::MODE_COLOR_TEMPERATURE,
        )
        .command(DeviceCommand::SetColorTemperature {
            mireds: target,
            transition,
        }),
    }
}

/// The limits of a colour temperature move or step: the command's limits where given (`0`
/// means "use the physical limit"), never beyond the physical ones.
fn temperature_limits(min: u16, max: u16, physical_min: u16, physical_max: u16) -> (u16, u16) {
    let min = if min == 0 {
        physical_min
    } else {
        min.max(physical_min)
    };
    let max = if max == 0 {
        physical_max
    } else {
        max.min(physical_max)
    };
    (min, max.max(min))
}

fn identify_command(command: CommandId, payload: &Value) -> Translation {
    match command {
        identify::IDENTIFY => {
            let Some(seconds) = u16_field(payload, 0) else {
                return Translation::status(Status::InvalidCommand);
            };
            Translation::success()
                .change(Attribute::IdentifyTime, seconds)
                .command(DeviceCommand::Identify {
                    duration: Duration::from_secs(u64::from(seconds)),
                })
        }
        identify::TRIGGER_EFFECT => {
            let (Some(effect), Some(variant)) = (u8_field(payload, 0), u8_field(payload, 1)) else {
                return Translation::status(Status::InvalidCommand);
            };
            Translation::success().command(DeviceCommand::TriggerEffect { effect, variant })
        }
        _ => Translation::status(Status::UnsupportedCommand),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;
    use core::time::Duration;

    use matter_rs_engine::{
        AttributeId, ClusterDescription, ClusterId, CommandId, EndpointId, Status, Value,
    };

    use super::{CommandOutcome, Translation, WriteOutcome, translate_command, translate_write};
    use crate::command::DeviceCommand;
    use crate::ids::{
        bridged_device_basic_information as bdbi, color_control as cc, identify,
        level_control as lc, on_off,
    };
    use crate::kinds::DeviceKind;
    use crate::model::{Attribute, DeviceKey, DeviceSpec, InitialValues};
    use crate::plan::{Numbers, PlannedEndpoint, plan_device};

    fn endpoint(kind: DeviceKind, initial: InitialValues) -> PlannedEndpoint {
        let spec = DeviceSpec::single(DeviceKey::new("dev").unwrap(), "Device", kind);
        plan_device(
            &spec,
            &initial,
            &Numbers {
                parent: None,
                parts: vec![EndpointId::new(2)],
            },
            &|_, _| None,
        )
        .unwrap()
        .remove(0)
    }

    fn light(on: bool, level: u8) -> PlannedEndpoint {
        endpoint(
            DeviceKind::DimmableLight,
            InitialValues::new()
                .value(Attribute::OnOff, on)
                .value(Attribute::CurrentLevel, Value::U8(level)),
        )
    }

    fn color_light(on: bool) -> PlannedEndpoint {
        endpoint(
            DeviceKind::ExtendedColorLight,
            InitialValues::new()
                .value(Attribute::OnOff, on)
                .value(Attribute::CurrentLevel, Value::U8(100))
                .value(Attribute::ColorTemperatureMireds, Value::U16(300))
                .value(Attribute::ColorTempPhysicalMinMireds, Value::U16(153))
                .value(Attribute::ColorTempPhysicalMaxMireds, Value::U16(500))
                .value(Attribute::CurrentHue, Value::U8(100))
                .value(Attribute::CurrentSaturation, Value::U8(200))
                .value(Attribute::CurrentX, Value::U16(1000))
                .value(Attribute::CurrentY, Value::U16(2000)),
        )
    }

    fn fields(fields: &[(u64, Value)]) -> Value {
        Value::Struct(fields.to_vec())
    }

    fn handled(outcome: CommandOutcome) -> Translation {
        match outcome {
            CommandOutcome::Handled(translation) => translation,
            CommandOutcome::Forward => panic!("forwarded"),
        }
    }

    fn changes(translation: &Translation) -> Vec<(Attribute, Value)> {
        translation
            .changes
            .iter()
            .map(|(c, a, v)| (Attribute::from_path(*c, *a), v.clone()))
            .collect()
    }

    fn gsc() -> Attribute {
        Attribute::Raw(on_off::CLUSTER, on_off::GLOBAL_SCENE_CONTROL)
    }

    #[test]
    fn on_off_commands_set_the_state() {
        let ep = light(false, 10);
        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::ON,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::Success);
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::OnOff, Value::Bool(true)),
                (gsc(), Value::Bool(true))
            ]
        );
        assert_eq!(t.commands, vec![DeviceCommand::SetOnOff { on: true }]);

        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::TOGGLE,
            &Value::Null,
        ));
        assert_eq!(changes(&t)[0], (Attribute::OnOff, Value::Bool(true)));

        let ep = light(true, 10);
        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::TOGGLE,
            &Value::Null,
        ));
        assert_eq!(changes(&t), vec![(Attribute::OnOff, Value::Bool(false))]);
        assert_eq!(t.commands, vec![DeviceCommand::SetOnOff { on: false }]);

        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::OFF_WITH_EFFECT,
            &fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
        ));
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::OnOff, Value::Bool(false)),
                (gsc(), Value::Bool(false))
            ]
        );
        assert_eq!(t.commands, vec![DeviceCommand::SetOnOff { on: false }]);
        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::OFF_WITH_EFFECT,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::InvalidCommand);

        let t = handled(translate_command(
            &light(false, 10),
            on_off::CLUSTER,
            on_off::ON_WITH_TIMED_OFF,
            &fields(&[(0, Value::U8(0)), (1, Value::U16(600)), (2, Value::U16(50))]),
        ));
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::OnOff, Value::Bool(true)),
                (gsc(), Value::Bool(true)),
                (Attribute::OnTime, Value::U16(600)),
                (Attribute::OffWaitTime, Value::U16(50)),
            ]
        );
        assert_eq!(t.commands, vec![DeviceCommand::SetOnOff { on: true }]);
    }

    #[test]
    fn on_with_on_level_moves_to_that_level() {
        let ep = endpoint(
            DeviceKind::DimmableLight,
            InitialValues::new()
                .value(Attribute::OnOff, false)
                .value(Attribute::CurrentLevel, Value::U8(10))
                .value(Attribute::OnLevel, Value::U8(200)),
        );
        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            on_off::ON,
            &Value::Null,
        ));
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::OnOff, Value::Bool(true)),
                (gsc(), Value::Bool(true)),
                (Attribute::CurrentLevel, Value::U8(200)),
            ]
        );
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 200,
                transition: None,
                on_off: Some(true)
            }]
        );
    }

    #[test]
    fn level_commands_translate_to_targets() {
        let ep = light(true, 100);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL,
            &fields(&[
                (0, Value::U8(50)),
                (1, Value::U16(20)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(changes(&t), vec![(Attribute::CurrentLevel, Value::U8(50))]);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 50,
                transition: Some(Duration::from_secs(2)),
                on_off: None
            }]
        );
        // Null transition means the device's default; level 0 is clamped to the minimum and
        // turns the light off with the on/off variant.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL_WITH_ON_OFF,
            &fields(&[
                (0, Value::U8(0)),
                (1, Value::Null),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::CurrentLevel, Value::U8(1)),
                (Attribute::OnOff, Value::Bool(false)),
            ]
        );
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 1,
                transition: None,
                on_off: Some(false)
            }]
        );
        // Move up runs to the maximum at the rate; 154 units at 77 per second take 2 s.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE,
            &fields(&[
                (0, Value::U8(0)),
                (1, Value::U8(77)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(changes(&t), vec![(Attribute::CurrentLevel, Value::U8(254))]);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 254,
                transition: Some(Duration::from_secs(2)),
                on_off: None
            }]
        );
        // Move down with on/off reaches the minimum and turns off; a null rate has no hint.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_WITH_ON_OFF,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::Null),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 1,
                transition: None,
                on_off: Some(false)
            }]
        );
        // Step down by 30 from 100.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::STEP,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(30)),
                (2, Value::U16(5)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(changes(&t), vec![(Attribute::CurrentLevel, Value::U8(70))]);
        // Step up beyond the maximum is clamped.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::STEP_WITH_ON_OFF,
            &fields(&[
                (0, Value::U8(0)),
                (1, Value::U8(200)),
                (2, Value::Null),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            changes(&t),
            vec![
                (Attribute::CurrentLevel, Value::U8(254)),
                (Attribute::OnOff, Value::Bool(true)),
            ]
        );
        // Stop targets the current level with a zero transition.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::STOP,
            &fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetLevel {
                level: 100,
                transition: Some(Duration::ZERO),
                on_off: None
            }]
        );
        // Malformed payloads are invalid commands.
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::InvalidCommand);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE,
            &fields(&[(0, Value::U8(7)), (1, Value::U8(1))]),
        ));
        assert_eq!(t.status, Status::InvalidCommand);
    }

    #[test]
    fn level_commands_without_on_off_respect_execute_if_off() {
        let ep = light(false, 100);
        let payload = fields(&[
            (0, Value::U8(50)),
            (1, Value::Null),
            (2, Value::U8(0)),
            (3, Value::U8(0)),
        ]);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL,
            &payload,
        ));
        assert_eq!(t.status, Status::Success);
        assert!(t.changes.is_empty());
        assert!(t.commands.is_empty());
        // The override sets ExecuteIfOff for this command.
        let payload = fields(&[
            (0, Value::U8(50)),
            (1, Value::Null),
            (2, Value::U8(1)),
            (3, Value::U8(1)),
        ]);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL,
            &payload,
        ));
        assert_eq!(changes(&t), vec![(Attribute::CurrentLevel, Value::U8(50))]);
        // The stored option does the same.
        let ep = endpoint(
            DeviceKind::DimmableLight,
            InitialValues::new()
                .value(Attribute::OnOff, false)
                .value(Attribute::CurrentLevel, Value::U8(100))
                .value(Attribute::LevelOptions, Value::U8(1)),
        );
        let payload = fields(&[
            (0, Value::U8(50)),
            (1, Value::Null),
            (2, Value::U8(0)),
            (3, Value::U8(0)),
        ]);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL,
            &payload,
        ));
        assert_eq!(changes(&t), vec![(Attribute::CurrentLevel, Value::U8(50))]);
        // With-on/off variants always execute.
        let ep = light(false, 100);
        let t = handled(translate_command(
            &ep,
            lc::CLUSTER,
            lc::MOVE_TO_LEVEL_WITH_ON_OFF,
            &payload,
        ));
        assert_eq!(changes(&t).len(), 2);
    }

    fn mode_changes(mode: u8) -> Vec<(Attribute, Value)> {
        vec![
            (Attribute::ColorMode, Value::U8(mode)),
            (
                Attribute::Raw(cc::CLUSTER, cc::ENHANCED_COLOR_MODE),
                Value::U8(mode),
            ),
        ]
    }

    #[test]
    fn color_temperature_commands_translate_to_targets() {
        let ep = color_light(true);
        // Clamped to the physical range.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_COLOR_TEMPERATURE,
            &fields(&[
                (0, Value::U16(100)),
                (1, Value::U16(10)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        let mut expected = vec![(Attribute::ColorTemperatureMireds, Value::U16(153))];
        expected.extend(mode_changes(cc::MODE_COLOR_TEMPERATURE));
        assert_eq!(changes(&t), expected);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorTemperature {
                mireds: 153,
                transition: Some(Duration::from_secs(1))
            }]
        );
        // Move up (warmer) to the command's maximum, 100 mireds at 50 per second.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_COLOR_TEMPERATURE,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U16(50)),
                (2, Value::U16(0)),
                (3, Value::U16(400)),
                (4, Value::U8(0)),
                (5, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorTemperature {
                mireds: 400,
                transition: Some(Duration::from_secs(2))
            }]
        );
        // Move stop holds the current value.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_COLOR_TEMPERATURE,
            &fields(&[
                (0, Value::U8(0)),
                (1, Value::U16(50)),
                (2, Value::U16(0)),
                (3, Value::U16(0)),
                (4, Value::U8(0)),
                (5, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorTemperature {
                mireds: 300,
                transition: Some(Duration::ZERO)
            }]
        );
        // Step down by 500 is clamped to the physical minimum.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STEP_COLOR_TEMPERATURE,
            &fields(&[
                (0, Value::U8(3)),
                (1, Value::U16(500)),
                (2, Value::U16(0)),
                (3, Value::U16(0)),
                (4, Value::U16(0)),
                (5, Value::U8(0)),
                (6, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorTemperature {
                mireds: 153,
                transition: Some(Duration::ZERO)
            }]
        );
        // StopMoveStep in colour temperature mode.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STOP_MOVE_STEP,
            &fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorTemperature {
                mireds: 300,
                transition: Some(Duration::ZERO)
            }]
        );
    }

    #[test]
    fn hue_saturation_and_xy_commands_translate_to_targets() {
        let ep = color_light(true);
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_HUE,
            &fields(&[
                (0, Value::U8(42)),
                (1, Value::U8(0)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        let mut expected = vec![(Attribute::CurrentHue, Value::U8(42))];
        expected.extend(mode_changes(cc::MODE_HUE_SATURATION));
        assert_eq!(changes(&t), expected);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetHue {
                hue: 42,
                transition: Some(Duration::ZERO)
            }]
        );
        // Step hue wraps around the circle: 100 + 200 = 300 -> 45.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STEP_HUE,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(200)),
                (2, Value::U8(10)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetHue {
                hue: 45,
                transition: Some(Duration::from_secs(1))
            }]
        );
        // Move hue down runs to 0 at 50 per second: 2 s.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_HUE,
            &fields(&[
                (0, Value::U8(3)),
                (1, Value::U8(50)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetHue {
                hue: 0,
                transition: Some(Duration::from_secs(2))
            }]
        );
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_SATURATION,
            &fields(&[
                (0, Value::U8(254)),
                (1, Value::U16(5)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetSaturation {
                saturation: 254,
                transition: Some(Duration::from_millis(500))
            }]
        );
        // Step saturation up is clamped at 254.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STEP_SATURATION,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(100)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetSaturation {
                saturation: 254,
                transition: Some(Duration::ZERO)
            }]
        );
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_SATURATION,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(27)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetSaturation {
                saturation: 254,
                transition: Some(Duration::from_secs(2))
            }]
        );
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_HUE_AND_SATURATION,
            &fields(&[
                (0, Value::U8(1)),
                (1, Value::U8(2)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetHueAndSaturation {
                hue: 1,
                saturation: 2,
                transition: Some(Duration::ZERO)
            }]
        );
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_COLOR,
            &fields(&[
                (0, Value::U16(30000)),
                (1, Value::U16(0xFFFF)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        let mut expected = vec![
            (Attribute::CurrentX, Value::U16(30000)),
            (Attribute::CurrentY, Value::U16(0xFEFF)),
        ];
        expected.extend(mode_changes(cc::MODE_XY));
        assert_eq!(changes(&t), expected);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorXy {
                x: 30000,
                y: 0xFEFF,
                transition: Some(Duration::ZERO)
            }]
        );
        // Step colour: x + 500, y - 5000 clamped at 0.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STEP_COLOR,
            &fields(&[
                (0, Value::I16(500)),
                (1, Value::I16(-5000)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorXy {
                x: 1500,
                y: 0,
                transition: Some(Duration::ZERO)
            }]
        );
        // Move colour: x towards the maximum at 1000 per second, y held.
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_COLOR,
            &fields(&[
                (0, Value::I16(1000)),
                (1, Value::I16(0)),
                (2, Value::U8(0)),
                (3, Value::U8(0)),
            ]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetColorXy {
                x: 0xFEFF,
                y: 2000,
                transition: Some(Duration::from_millis((0xFEFF - 1000) * 1000 / 1000))
            }]
        );
        // StopMoveStep follows the colour mode.
        let mut ep = color_light(true);
        ep.values.insert(
            Attribute::ColorMode.path(),
            Value::U8(cc::MODE_HUE_SATURATION),
        );
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::STOP_MOVE_STEP,
            &fields(&[(0, Value::U8(0)), (1, Value::U8(0))]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::SetHueAndSaturation {
                hue: 100,
                saturation: 200,
                transition: Some(Duration::ZERO)
            }]
        );
    }

    #[test]
    fn color_commands_respect_execute_if_off_and_features() {
        let ep = color_light(false);
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_HUE,
            &fields(&[
                (0, Value::U8(42)),
                (1, Value::U8(0)),
                (2, Value::U16(0)),
                (3, Value::U8(0)),
                (4, Value::U8(0)),
            ]),
        ));
        assert_eq!(t.status, Status::Success);
        assert!(t.commands.is_empty());
        // A colour temperature light has no hue commands: unsupported by the engine already, and
        // by the bridge as a second line.
        let ct = endpoint(
            DeviceKind::ColorTemperatureLight,
            InitialValues::new()
                .value(Attribute::OnOff, true)
                .value(Attribute::CurrentLevel, Value::U8(100))
                .value(Attribute::ColorTemperatureMireds, Value::U16(300))
                .value(Attribute::ColorTempPhysicalMinMireds, Value::U16(153))
                .value(Attribute::ColorTempPhysicalMaxMireds, Value::U16(500)),
        );
        let t = handled(translate_command(
            &ct,
            cc::CLUSTER,
            cc::MOVE_TO_HUE,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::UnsupportedCommand);
    }

    #[test]
    fn identify_commands() {
        let ep = light(true, 10);
        let t = handled(translate_command(
            &ep,
            identify::CLUSTER,
            identify::IDENTIFY,
            &fields(&[(0, Value::U16(15))]),
        ));
        assert_eq!(changes(&t), vec![(Attribute::IdentifyTime, Value::U16(15))]);
        assert_eq!(
            t.commands,
            vec![DeviceCommand::Identify {
                duration: Duration::from_secs(15)
            }]
        );
        let t = handled(translate_command(
            &ep,
            identify::CLUSTER,
            identify::TRIGGER_EFFECT,
            &fields(&[(0, Value::U8(1)), (1, Value::U8(0))]),
        ));
        assert_eq!(
            t.commands,
            vec![DeviceCommand::TriggerEffect {
                effect: 1,
                variant: 0
            }]
        );
        let t = handled(translate_command(
            &ep,
            identify::CLUSTER,
            identify::IDENTIFY,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::InvalidCommand);
    }

    #[test]
    fn unknown_commands_and_clusters_are_unsupported() {
        let ep = light(true, 10);
        let t = handled(translate_command(
            &ep,
            on_off::CLUSTER,
            CommandId::new(0x7F),
            &Value::Null,
        ));
        assert_eq!(t.status, Status::UnsupportedCommand);
        let t = handled(translate_command(
            &ep,
            cc::CLUSTER,
            cc::MOVE_TO_HUE,
            &Value::Null,
        ));
        assert_eq!(t.status, Status::UnsupportedCluster);
    }

    #[test]
    fn extra_cluster_commands_and_writes_are_forwarded() {
        let vendor = ClusterId::new(0xFFF1_FC01);
        let mut spec = DeviceSpec::single(
            DeviceKey::new("dev").unwrap(),
            "Device",
            DeviceKind::OnOffLight,
        );
        spec.parts[0].extra_clusters.push(ClusterDescription {
            id: vendor,
            revision: 1,
            feature_map: 0,
            attributes: vec![AttributeId::new(0)],
            accepted_commands: vec![CommandId::new(1)],
            generated_commands: vec![],
            events: vec![],
        });
        let ep = plan_device(
            &spec,
            &InitialValues::new()
                .value(Attribute::OnOff, true)
                .value(Attribute::Raw(vendor, AttributeId::new(0)), Value::U8(0)),
            &Numbers {
                parent: None,
                parts: vec![EndpointId::new(2)],
            },
            &|_, _| None,
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            translate_command(&ep, vendor, CommandId::new(1), &Value::Null),
            CommandOutcome::Forward
        );
        assert_eq!(
            translate_command(&ep, vendor, CommandId::new(2), &Value::Null),
            CommandOutcome::Handled(Translation::status(Status::UnsupportedCommand))
        );
        assert_eq!(
            translate_write(&ep, vendor, AttributeId::new(0), &Value::U8(1)),
            WriteOutcome::Forward
        );
    }

    #[test]
    fn writes_are_checked_stored_and_reported() {
        let ep = light(true, 10);
        // OnLevel: persisted, within range, width normalised.
        assert_eq!(
            translate_write(&ep, lc::CLUSTER, lc::ON_LEVEL, &Value::U16(77)),
            WriteOutcome::Accepted {
                stored: Value::U8(77),
                persist: true,
                commands: vec![DeviceCommand::Written {
                    attribute: Attribute::OnLevel,
                    value: Value::U8(77)
                }],
            }
        );
        assert_eq!(
            translate_write(&ep, lc::CLUSTER, lc::ON_LEVEL, &Value::Null),
            WriteOutcome::Accepted {
                stored: Value::Null,
                persist: true,
                commands: vec![DeviceCommand::Written {
                    attribute: Attribute::OnLevel,
                    value: Value::Null
                }],
            }
        );
        assert_eq!(
            translate_write(&ep, lc::CLUSTER, lc::ON_LEVEL, &Value::U8(0)),
            WriteOutcome::Rejected(Status::ConstraintError)
        );
        assert_eq!(
            translate_write(&ep, lc::CLUSTER, lc::ON_LEVEL, &Value::Bool(true)),
            WriteOutcome::Rejected(Status::InvalidDataType)
        );
        // StartUpOnOff: enum range.
        assert_eq!(
            translate_write(&ep, on_off::CLUSTER, on_off::START_UP_ON_OFF, &Value::U8(3)),
            WriteOutcome::Rejected(Status::ConstraintError)
        );
        assert!(matches!(
            translate_write(&ep, on_off::CLUSTER, on_off::START_UP_ON_OFF, &Value::U8(1)),
            WriteOutcome::Accepted { persist: true, .. }
        ));
        // OnTime is stored but not persisted.
        assert!(matches!(
            translate_write(&ep, on_off::CLUSTER, on_off::ON_TIME, &Value::U16(5)),
            WriteOutcome::Accepted { persist: false, .. }
        ));
        // Read-only and unknown attributes.
        assert_eq!(
            translate_write(&ep, on_off::CLUSTER, on_off::ON_OFF, &Value::Bool(true)),
            WriteOutcome::Rejected(Status::UnsupportedWrite)
        );
        assert_eq!(
            translate_write(
                &ep,
                on_off::CLUSTER,
                AttributeId::new(0x77),
                &Value::Bool(true)
            ),
            WriteOutcome::Rejected(Status::UnsupportedAttribute)
        );
        // Null where not allowed is a constraint error.
        assert_eq!(
            translate_write(&ep, lc::CLUSTER, lc::OPTIONS, &Value::Null),
            WriteOutcome::Rejected(Status::ConstraintError)
        );
        // IdentifyTime starts identification.
        assert_eq!(
            translate_write(
                &ep,
                identify::CLUSTER,
                identify::IDENTIFY_TIME,
                &Value::U8(3)
            ),
            WriteOutcome::Accepted {
                stored: Value::U16(3),
                persist: false,
                commands: vec![DeviceCommand::Identify {
                    duration: Duration::from_secs(3)
                }],
            }
        );
    }

    #[test]
    fn node_label_write_is_a_rename() {
        let ep = light(true, 10);
        assert_eq!(
            translate_write(
                &ep,
                bdbi::CLUSTER,
                bdbi::NODE_LABEL,
                &Value::from("Kitchen")
            ),
            WriteOutcome::Accepted {
                stored: Value::from("Kitchen"),
                persist: true,
                commands: vec![DeviceCommand::Rename(alloc::string::String::from(
                    "Kitchen"
                ))],
            }
        );
        assert_eq!(
            translate_write(
                &ep,
                bdbi::CLUSTER,
                bdbi::NODE_LABEL,
                &Value::from("x".repeat(33).as_str())
            ),
            WriteOutcome::Rejected(Status::ConstraintError)
        );
        assert_eq!(
            translate_write(&ep, bdbi::CLUSTER, bdbi::NODE_LABEL, &Value::U8(1)),
            WriteOutcome::Rejected(Status::InvalidDataType)
        );
        assert_eq!(
            translate_write(&ep, bdbi::CLUSTER, bdbi::REACHABLE, &Value::Bool(false)),
            WriteOutcome::Rejected(Status::UnsupportedWrite)
        );
    }
}
