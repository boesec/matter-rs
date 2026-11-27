//! From adapter-reported device events to cluster events.

use alloc::vec;

use matter_rs_engine::{ClusterId, EventId, Value};

use crate::ids::switch;
use crate::model::DeviceEvent;

/// Translates a device event into the cluster, event id and payload to emit, plus the
/// `CurrentPosition` the Switch cluster takes afterwards. `None` for [`DeviceEvent::Raw`], which
/// carries its ids itself.
///
/// The Switch cluster's events each carry the position as their first field; the multi-press
/// events add the press count as the second. A press moves `CurrentPosition` to the pressed
/// position, a release returns it to the idle position.
pub(crate) fn translate_event(
    event: &DeviceEvent,
) -> Option<(ClusterId, EventId, Value, Option<u8>)> {
    let (id, payload, position) = match event {
        DeviceEvent::InitialPress { position } => (
            switch::INITIAL_PRESS,
            vec![(0, Value::U8(*position))],
            Some(*position),
        ),
        DeviceEvent::LongPress { position } => (
            switch::LONG_PRESS,
            vec![(0, Value::U8(*position))],
            Some(*position),
        ),
        DeviceEvent::ShortRelease { previous_position } => (
            switch::SHORT_RELEASE,
            vec![(0, Value::U8(*previous_position))],
            Some(switch::IDLE_POSITION),
        ),
        DeviceEvent::LongRelease { previous_position } => (
            switch::LONG_RELEASE,
            vec![(0, Value::U8(*previous_position))],
            Some(switch::IDLE_POSITION),
        ),
        DeviceEvent::MultiPressOngoing { position, count } => (
            switch::MULTI_PRESS_ONGOING,
            vec![(0, Value::U8(*position)), (1, Value::U8(*count))],
            Some(*position),
        ),
        DeviceEvent::MultiPressComplete {
            previous_position,
            count,
        } => (
            switch::MULTI_PRESS_COMPLETE,
            vec![(0, Value::U8(*previous_position)), (1, Value::U8(*count))],
            Some(switch::IDLE_POSITION),
        ),
        _ => return None,
    };
    Some((switch::CLUSTER, id, Value::Struct(payload), position))
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use matter_rs_engine::Value;

    use super::translate_event;
    use crate::ids::switch;
    use crate::model::DeviceEvent;

    #[test]
    fn switch_events_carry_position_and_count() {
        assert_eq!(
            translate_event(&DeviceEvent::InitialPress { position: 1 }),
            Some((
                switch::CLUSTER,
                switch::INITIAL_PRESS,
                Value::Struct(vec![(0, Value::U8(1))]),
                Some(1)
            ))
        );
        assert_eq!(
            translate_event(&DeviceEvent::ShortRelease {
                previous_position: 1
            }),
            Some((
                switch::CLUSTER,
                switch::SHORT_RELEASE,
                Value::Struct(vec![(0, Value::U8(1))]),
                Some(0)
            ))
        );
        assert_eq!(
            translate_event(&DeviceEvent::MultiPressComplete {
                previous_position: 1,
                count: 2
            }),
            Some((
                switch::CLUSTER,
                switch::MULTI_PRESS_COMPLETE,
                Value::Struct(vec![(0, Value::U8(1)), (1, Value::U8(2))]),
                Some(0)
            ))
        );
        assert_eq!(
            translate_event(&DeviceEvent::Raw {
                cluster: switch::CLUSTER,
                event: switch::LONG_PRESS,
                payload: Value::Null
            }),
            None
        );
    }
}
