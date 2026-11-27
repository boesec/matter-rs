//! Endpoint numbers that stay stable across restarts.
//!
//! Every endpoint of a bridged device has a *slot*: the device key plus either the part index
//! or, for the parent endpoint of a composed device, a parent marker. The numbering maps slots
//! to endpoint numbers and remembers the next number to hand out:
//!
//! - a slot that was numbered before gets its old number back, however long ago and whatever
//!   was added in between;
//! - a new slot gets the next number after the last one assigned, wrapping from `0xFFFE` to
//!   [`FIRST_BRIDGED_ENDPOINT`] and skipping numbers that are still mapped;
//! - removing a device does not touch the map, so its numbers stay reserved until the device
//!   is forgotten through the bridge.
//!
//! The aggregator is endpoint [`AGGREGATOR`], the first bridged endpoint is 2. Numbers never
//! appear in the configuration; they live in the storage under `bridge/endpoints/map` (the
//! slot map) and `bridge/endpoints/next` (the counter). The map is a version byte `1` followed
//! by one entry per slot: the number (`u16`), the part (`u16`, `0xFFFF` for the parent), the key
//! length (`u16`) and the key bytes, all big-endian; the counter is two big-endian bytes.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use matter_rs_engine::EndpointId;

use crate::error::Error;
use crate::model::{DeviceKey, PartIndex};

/// The aggregator endpoint of the bridge.
pub const AGGREGATOR: EndpointId = EndpointId::new(1);

/// The first endpoint number a bridged device can get.
pub const FIRST_BRIDGED_ENDPOINT: u16 = 2;

/// The last usable endpoint number; `0xFFFF` is the wildcard.
pub const LAST_BRIDGED_ENDPOINT: u16 = 0xFFFE;

/// The storage key of the slot map.
pub const MAP_KEY: &str = "bridge/endpoints/map";

/// The storage key of the next-number counter.
pub const NEXT_KEY: &str = "bridge/endpoints/next";

/// The version byte the slot map starts with.
const MAP_VERSION: u8 = 1;

/// One numbered endpoint of a device: a part, or the parent endpoint of a composed device.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Slot {
    /// The device.
    pub key: DeviceKey,
    /// The part index, or [`Slot::PARENT`].
    pub part: u16,
}

impl Slot {
    /// The part value of the parent endpoint of a composed device.
    pub const PARENT: u16 = u16::MAX;

    /// The slot of a part.
    pub(crate) fn part(key: &DeviceKey, part: PartIndex) -> Self {
        Self {
            key: key.clone(),
            part,
        }
    }

    /// The slot of the parent endpoint of a composed device.
    pub(crate) fn parent(key: &DeviceKey) -> Self {
        Self {
            key: key.clone(),
            part: Self::PARENT,
        }
    }
}

/// The slot map and the counter. Pure data: the caller persists [`Numbering::encode_map`] and
/// [`Numbering::encode_next`] after every change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Numbering {
    map: BTreeMap<Slot, u16>,
    next: u16,
}

impl Default for Numbering {
    fn default() -> Self {
        Self::new()
    }
}

impl Numbering {
    /// An empty numbering that starts at [`FIRST_BRIDGED_ENDPOINT`].
    pub(crate) fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            next: FIRST_BRIDGED_ENDPOINT,
        }
    }

    /// Rebuilds the numbering from its persisted form; `None` for a part that was never
    /// persisted.
    ///
    /// # Errors
    ///
    /// [`Error::CorruptState`] when either value cannot be decoded.
    pub(crate) fn decode(map: Option<&[u8]>, next: Option<&[u8]>) -> Result<Self, Error> {
        let mut numbering = Self::new();
        if let Some(bytes) = map {
            numbering.map = decode_map(bytes).map_err(|reason| Error::CorruptState {
                key: String::from(MAP_KEY),
                reason,
            })?;
        }
        if let Some(bytes) = next {
            let array: [u8; 2] = bytes.try_into().map_err(|_| Error::CorruptState {
                key: String::from(NEXT_KEY),
                reason: String::from("the counter is not two bytes"),
            })?;
            numbering.next = u16::from_be_bytes(array);
        }
        if !(FIRST_BRIDGED_ENDPOINT..=LAST_BRIDGED_ENDPOINT).contains(&numbering.next) {
            return Err(Error::CorruptState {
                key: String::from(NEXT_KEY),
                reason: String::from("the counter is outside the endpoint range"),
            });
        }
        Ok(numbering)
    }

    /// The number of a slot, if it has one.
    #[cfg(test)]
    pub(crate) fn get(&self, slot: &Slot) -> Option<EndpointId> {
        self.map.get(slot).copied().map(EndpointId::new)
    }

    /// The number of a slot: its old one when it has one, otherwise the next free number.
    ///
    /// # Errors
    ///
    /// [`Error::EndpointsExhausted`] when every number is reserved.
    pub(crate) fn assign(&mut self, slot: Slot) -> Result<EndpointId, Error> {
        if let Some(number) = self.map.get(&slot) {
            return Ok(EndpointId::new(*number));
        }
        let span = usize::from(LAST_BRIDGED_ENDPOINT - FIRST_BRIDGED_ENDPOINT) + 1;
        if self.map.len() >= span {
            return Err(Error::EndpointsExhausted);
        }
        loop {
            let candidate = self.next;
            self.next = if candidate >= LAST_BRIDGED_ENDPOINT {
                FIRST_BRIDGED_ENDPOINT
            } else {
                candidate + 1
            };
            if !self.map.values().any(|number| *number == candidate) {
                self.map.insert(slot, candidate);
                return Ok(EndpointId::new(candidate));
            }
        }
    }

    /// Releases every slot of a device; the numbers may be handed out again.
    pub(crate) fn forget(&mut self, key: &DeviceKey) -> Vec<EndpointId> {
        let doomed: Vec<Slot> = self
            .map
            .keys()
            .filter(|slot| slot.key == *key)
            .cloned()
            .collect();
        doomed
            .iter()
            .filter_map(|slot| self.map.remove(slot))
            .map(EndpointId::new)
            .collect()
    }

    /// Keeps only the slots of `live` devices and returns the keys of the devices that were
    /// forgotten. The counter does not move.
    pub(crate) fn retain(&mut self, live: &[DeviceKey]) -> Vec<DeviceKey> {
        let mut forgotten: Vec<DeviceKey> = self
            .map
            .keys()
            .filter(|slot| !live.contains(&slot.key))
            .map(|slot| slot.key.clone())
            .collect();
        forgotten.dedup();
        self.map.retain(|slot, _| live.contains(&slot.key));
        forgotten
    }

    /// Every reserved number, in slot order.
    pub(crate) fn reserved(&self) -> impl Iterator<Item = EndpointId> + '_ {
        self.map.values().copied().map(EndpointId::new)
    }

    /// The next number the numbering would hand out to a slot without one, if that number is
    /// free.
    #[cfg(test)]
    pub(crate) fn next(&self) -> u16 {
        self.next
    }

    /// The slot map in its persisted form: a version byte `1`, then one entry per slot with
    /// the number (`u16`), the part (`u16`, `0xFFFF` for the parent), the key length (`u16`)
    /// and the key bytes, all big-endian, in slot order.
    pub(crate) fn encode_map(&self) -> Vec<u8> {
        let mut out = alloc::vec![MAP_VERSION];
        for (slot, number) in &self.map {
            out.extend(number.to_be_bytes());
            out.extend(slot.part.to_be_bytes());
            let key = slot.key.as_str().as_bytes();
            // Keys are at most 64 bytes, see `DeviceKey::new`.
            out.extend(u16::try_from(key.len()).unwrap_or(u16::MAX).to_be_bytes());
            out.extend(key);
        }
        out
    }

    /// The counter in its persisted form: two big-endian bytes.
    pub(crate) fn encode_next(&self) -> [u8; 2] {
        self.next.to_be_bytes()
    }
}

fn decode_map(bytes: &[u8]) -> Result<BTreeMap<Slot, u16>, String> {
    let Some((&version, mut rest)) = bytes.split_first() else {
        return Err(String::from("the map is empty"));
    };
    if version != MAP_VERSION {
        return Err(alloc::format!("unknown map version {version}"));
    }
    let mut map = BTreeMap::new();
    let mut take = |n: usize| -> Result<&[u8], String> {
        if rest.len() < n {
            return Err(String::from("truncated entry"));
        }
        let (head, tail) = rest.split_at(n);
        rest = tail;
        Ok(head)
    };
    while let Ok(number) = take(2) {
        let number = u16::from_be_bytes([number[0], number[1]]);
        let part = take(2)?;
        let part = u16::from_be_bytes([part[0], part[1]]);
        let length = take(2)?;
        let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
        let key =
            core::str::from_utf8(take(length)?).map_err(|_| String::from("key is not UTF-8"))?;
        let key = DeviceKey::new(key).map_err(|error| alloc::format!("{error}"))?;
        if !(FIRST_BRIDGED_ENDPOINT..=LAST_BRIDGED_ENDPOINT).contains(&number) {
            return Err(alloc::format!(
                "number {number} is outside the endpoint range"
            ));
        }
        if map.values().any(|n| *n == number) {
            return Err(alloc::format!("number {number} is mapped twice"));
        }
        map.insert(Slot { key, part }, number);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeSet;
    use alloc::vec::Vec;

    use matter_rs_engine::EndpointId;
    use proptest::prelude::*;

    use super::{
        FIRST_BRIDGED_ENDPOINT, LAST_BRIDGED_ENDPOINT, MAP_KEY, NEXT_KEY, Numbering, Slot,
    };
    use crate::error::Error;
    use crate::model::DeviceKey;

    fn key(name: &str) -> DeviceKey {
        DeviceKey::new(name).unwrap()
    }

    #[test]
    fn numbers_start_at_two_and_count_up() {
        let mut numbering = Numbering::new();
        assert_eq!(
            numbering.assign(Slot::part(&key("a"), 0)).unwrap(),
            EndpointId::new(2)
        );
        assert_eq!(
            numbering.assign(Slot::parent(&key("b"))).unwrap(),
            EndpointId::new(3)
        );
        assert_eq!(
            numbering.assign(Slot::part(&key("b"), 0)).unwrap(),
            EndpointId::new(4)
        );
        // A known slot gets its number back and the counter does not move.
        assert_eq!(
            numbering.assign(Slot::part(&key("a"), 0)).unwrap(),
            EndpointId::new(2)
        );
        assert_eq!(numbering.next(), 5);
        assert_eq!(
            numbering.get(&Slot::part(&key("a"), 0)),
            Some(EndpointId::new(2))
        );
        assert_eq!(numbering.get(&Slot::part(&key("a"), 1)), None);
    }

    #[test]
    fn forget_releases_every_slot_of_the_device() {
        let mut numbering = Numbering::new();
        numbering.assign(Slot::parent(&key("b"))).unwrap();
        numbering.assign(Slot::part(&key("b"), 0)).unwrap();
        numbering.assign(Slot::part(&key("b"), 1)).unwrap();
        numbering.assign(Slot::part(&key("c"), 0)).unwrap();
        let released = numbering.forget(&key("b"));
        assert_eq!(released.len(), 3);
        assert_eq!(
            numbering.reserved().collect::<Vec<_>>(),
            alloc::vec![EndpointId::new(5)]
        );
        assert!(numbering.forget(&key("b")).is_empty());
        // The released numbers are not reused before the counter wraps.
        assert_eq!(
            numbering.assign(Slot::part(&key("b"), 0)).unwrap(),
            EndpointId::new(6)
        );
    }

    #[test]
    fn numbering_wraps_and_skips_used() {
        let mut numbering = Numbering::new();
        numbering.next = LAST_BRIDGED_ENDPOINT - 1;
        numbering.assign(Slot::part(&key("old"), 0)).unwrap(); // 0xFFFD
        numbering.assign(Slot::part(&key("a"), 0)).unwrap(); // 0xFFFE
        assert_eq!(numbering.next(), FIRST_BRIDGED_ENDPOINT);
        // Pretend 2 and 3 are still reserved.
        numbering.map.insert(Slot::part(&key("two"), 0), 2);
        numbering.map.insert(Slot::part(&key("three"), 0), 3);
        assert_eq!(
            numbering.assign(Slot::part(&key("b"), 0)).unwrap(),
            EndpointId::new(4)
        );
        assert_eq!(numbering.next(), 5);
    }

    #[test]
    fn exhaustion_is_an_error() {
        let mut numbering = Numbering::new();
        for n in FIRST_BRIDGED_ENDPOINT..=LAST_BRIDGED_ENDPOINT {
            numbering.map.insert(Slot::part(&key("k"), n), n);
        }
        assert_eq!(
            numbering.assign(Slot::part(&key("new"), 0)),
            Err(Error::EndpointsExhausted)
        );
    }

    #[test]
    fn persisted_form_round_trips() {
        let mut numbering = Numbering::new();
        numbering.assign(Slot::parent(&key("b"))).unwrap();
        numbering.assign(Slot::part(&key("b"), 0)).unwrap();
        numbering
            .assign(Slot::part(&key("a-long-key.with:chars"), 7))
            .unwrap();
        let map = numbering.encode_map();
        let next = numbering.encode_next();
        let decoded = Numbering::decode(Some(&map), Some(&next)).unwrap();
        assert_eq!(decoded, numbering);
        assert_eq!(map[0], 1);
        assert_eq!(Numbering::decode(None, None).unwrap(), Numbering::new());
    }

    #[test]
    fn corrupt_state_is_reported_by_key() {
        assert!(matches!(
            Numbering::decode(Some(&[]), None),
            Err(Error::CorruptState { key, .. }) if key == MAP_KEY
        ));
        assert!(matches!(
            Numbering::decode(Some(&[9]), None),
            Err(Error::CorruptState { reason, .. }) if reason.contains("version")
        ));
        assert!(matches!(
            Numbering::decode(Some(&[1, 0, 2, 0]), None),
            Err(Error::CorruptState { reason, .. }) if reason.contains("truncated")
        ));
        // Two slots with the same number.
        let mut bytes = alloc::vec![1u8];
        for name in [b"a", b"b"] {
            bytes.extend([0, 2, 0, 0, 0, 1]);
            bytes.extend(name);
        }
        assert!(matches!(
            Numbering::decode(Some(&bytes), None),
            Err(Error::CorruptState { reason, .. }) if reason.contains("twice")
        ));
        assert!(matches!(
            Numbering::decode(None, Some(&[1])),
            Err(Error::CorruptState { key, .. }) if key == NEXT_KEY
        ));
        assert!(matches!(
            Numbering::decode(None, Some(&[0, 1])),
            Err(Error::CorruptState { reason, .. }) if reason.contains("range")
        ));
    }

    /// One step of a random numbering history.
    #[derive(Debug, Clone)]
    enum Op {
        Assign(u8, u8),
        Forget(u8),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0..6u8, 0..3u8).prop_map(|(k, p)| Op::Assign(k, p)),
            1 => (0..6u8).prop_map(Op::Forget),
        ]
    }

    fn slot_of(k: u8, p: u8) -> Slot {
        Slot::part(&key(&alloc::format!("device-{k}")), u16::from(p))
    }

    proptest! {
        /// Keys in, numbers out: stable, unique, monotonic (modulo the wrap), reserved until
        /// forgotten, and the persisted form carries everything.
        #[test]
        fn numbering_properties(
            ops in proptest::collection::vec(op(), 1..40),
            start in FIRST_BRIDGED_ENDPOINT..=LAST_BRIDGED_ENDPOINT,
        ) {
            let span = u32::from(LAST_BRIDGED_ENDPOINT - FIRST_BRIDGED_ENDPOINT) + 1;
            // The position of a number on the ring that starts at `start`.
            let position = |number: u16| {
                (u32::from(number) + span - u32::from(start)) % span
            };
            let mut numbering = Numbering::new();
            numbering.next = start;
            let mut last_fresh: Option<u32> = None;
            let mut seen: alloc::collections::BTreeMap<Slot, u16> = Default::default();
            for op in ops {
                match op {
                    Op::Assign(k, p) => {
                        let slot = slot_of(k, p);
                        let before = numbering.get(&slot);
                        let number = numbering.assign(slot.clone()).unwrap().raw();
                        prop_assert!((FIRST_BRIDGED_ENDPOINT..=LAST_BRIDGED_ENDPOINT).contains(&number));
                        match before {
                            // Stable: a known slot gets its number back.
                            Some(old) => prop_assert_eq!(old.raw(), number),
                            None => {
                                // Monotonic: every fresh number lies further along the ring
                                // than the fresh number before it.
                                if let Some(last) = last_fresh {
                                    prop_assert!(position(number) > last, "{number} after position {last}");
                                }
                                last_fresh = Some(position(number));
                            }
                        }
                        // A slot that was never forgotten keeps its first number.
                        if let Some(first) = seen.get(&slot) {
                            prop_assert_eq!(*first, number);
                        }
                        seen.insert(slot, number);
                    }
                    Op::Forget(k) => {
                        let forgotten = key(&alloc::format!("device-{k}"));
                        numbering.forget(&forgotten);
                        seen.retain(|slot, _| slot.key != forgotten);
                        prop_assert_eq!(numbering.reserved().count(), seen.len());
                    }
                }
                // Unique: no two reserved slots share a number.
                let numbers: Vec<u16> = numbering.reserved().map(EndpointId::raw).collect();
                let distinct: BTreeSet<u16> = numbers.iter().copied().collect();
                prop_assert_eq!(numbers.len(), distinct.len());
                // The persisted form round trips.
                let decoded = Numbering::decode(Some(&numbering.encode_map()), Some(&numbering.encode_next())).unwrap();
                prop_assert_eq!(&decoded, &numbering);
            }
        }
    }
}
