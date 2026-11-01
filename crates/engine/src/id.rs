//! The identifiers of the Matter data model as newtypes.
//!
//! Each id wraps the integer width the Matter specification gives it. The newtypes keep the
//! different id spaces apart at compile time (an endpoint id cannot be passed where a cluster id
//! is expected) and carry no semantics beyond that: the facade does not know which cluster an id
//! names.

use core::fmt;

/// Generates an id newtype with the constructor, accessor and conversions every id shares.
///
/// `$display` is the format string for [`fmt::Display`]; ids that the specification writes in
/// hexadecimal (clusters, attributes, commands, events, device types, vendor and product ids) use
/// `0x{:04X}`, ids that are read as plain numbers (endpoints, fabric indexes) use `{}`.
macro_rules! id {
    ($(#[$doc:meta])* $name:ident($raw:ty), $display:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name($raw);

        impl $name {
            /// Wraps a raw id.
            #[must_use]
            pub const fn new(raw: $raw) -> Self {
                Self(raw)
            }

            /// Returns the raw id.
            #[must_use]
            pub const fn raw(self) -> $raw {
                self.0
            }
        }

        impl From<$raw> for $name {
            fn from(raw: $raw) -> Self {
                Self(raw)
            }
        }

        impl From<$name> for $raw {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, $display, self.0)
            }
        }
    };
}

id! {
    /// Id of an endpoint on a node (16 bits). Endpoint 0 is the root endpoint.
    EndpointId(u16), "{}"
}

id! {
    /// Id of a cluster (32 bits: a 16-bit vendor prefix and a 16-bit cluster id).
    ClusterId(u32), "0x{:04X}"
}

id! {
    /// Id of an attribute within a cluster (32 bits).
    AttributeId(u32), "0x{:04X}"
}

id! {
    /// Id of a command within a cluster (32 bits).
    CommandId(u32), "0x{:04X}"
}

id! {
    /// Id of an event within a cluster (32 bits).
    EventId(u32), "0x{:04X}"
}

id! {
    /// Id of a device type (32 bits).
    DeviceTypeId(u32), "0x{:04X}"
}

id! {
    /// Index of a fabric on this node (8 bits, 1-based; 0 is never a valid fabric).
    FabricIndex(u8), "{}"
}

id! {
    /// Vendor id assigned by the Connectivity Standards Alliance (16 bits).
    VendorId(u16), "0x{:04X}"
}

id! {
    /// Product id chosen by the vendor (16 bits).
    ProductId(u16), "0x{:04X}"
}

/// The root endpoint of every node; it carries the node-wide utility clusters.
pub const ROOT_ENDPOINT: EndpointId = EndpointId::new(0);

impl AttributeId {
    /// First id of the global attribute range (`0xF000..=0xFFFE`).
    const GLOBAL_START: u32 = 0xF000;
    /// Last id of the global attribute range.
    const GLOBAL_END: u32 = 0xFFFE;

    /// Whether the id lies in the global attribute range that every cluster shares.
    ///
    /// Global attributes (`ClusterRevision`, `FeatureMap`, `AttributeList`,
    /// `AcceptedCommandList`, `GeneratedCommandList`, ...) are derived by the engine from a
    /// cluster description; the product neither lists nor sets them.
    #[must_use]
    pub const fn is_global(self) -> bool {
        self.0 >= Self::GLOBAL_START && self.0 <= Self::GLOBAL_END
    }
}

#[cfg(test)]
mod tests {
    use alloc::format;

    use super::{AttributeId, ClusterId, EndpointId, FabricIndex, ROOT_ENDPOINT, VendorId};

    #[test]
    fn id_round_trips_through_raw() {
        let id = ClusterId::new(0x0006);
        assert_eq!(id.raw(), 0x0006);
        assert_eq!(ClusterId::from(0x0006_u32), id);
        assert_eq!(u32::from(id), 0x0006);
    }

    #[test]
    fn hexadecimal_ids_display_with_prefix() {
        assert_eq!(format!("{}", ClusterId::new(0x0006)), "0x0006");
        assert_eq!(format!("{}", AttributeId::new(0x1_0000)), "0x10000");
        assert_eq!(format!("{}", VendorId::new(0xFFF1)), "0xFFF1");
    }

    #[test]
    fn plain_ids_display_as_numbers() {
        assert_eq!(format!("{}", EndpointId::new(12)), "12");
        assert_eq!(format!("{}", FabricIndex::new(1)), "1");
        assert_eq!(ROOT_ENDPOINT, EndpointId::new(0));
    }

    #[test]
    fn global_attribute_range_is_detected() {
        assert!(AttributeId::new(0xFFFD).is_global());
        assert!(AttributeId::new(0xF000).is_global());
        assert!(!AttributeId::new(0xFFFF).is_global());
        assert!(!AttributeId::new(0x0000).is_global());
        assert!(!AttributeId::new(0x1_F000).is_global());
    }

    #[test]
    fn ids_order_by_raw_value() {
        assert!(EndpointId::new(1) < EndpointId::new(2));
    }
}
