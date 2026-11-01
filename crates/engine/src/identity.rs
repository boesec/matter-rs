//! What a node is: its identity, its network settings, its attestation credentials and its
//! role.

use alloc::string::String;
use alloc::vec::Vec;

use crate::id::{EndpointId, ProductId, VendorId};
use crate::secret::Secret;

/// The default Matter UDP port.
pub const MATTER_PORT: u16 = 5540;

/// The largest value of the 12-bit commissioning discriminator.
pub const MAX_DISCRIMINATOR: u16 = 0x0FFF;

/// The largest valid setup passcode (`99999998`); passcode `0` is undefined.
pub const MAX_PASSCODE: u32 = 99_999_998;

/// The passcodes the Matter specification forbids because they are too easy to guess.
pub const INVALID_PASSCODES: [u32; 10] = [
    11_111_111, 22_222_222, 33_333_333, 44_444_444, 55_555_555, 66_666_666, 77_777_777, 88_888_888,
    12_345_678, 87_654_321,
];

/// Whether `passcode` is a setup passcode the specification allows: between 1 and
/// [`MAX_PASSCODE`] and not one of [`INVALID_PASSCODES`].
#[must_use]
pub fn is_valid_passcode(passcode: u32) -> bool {
    (1..=MAX_PASSCODE).contains(&passcode) && !INVALID_PASSCODES.contains(&passcode)
}

/// The identity of the node: what it reports in Basic Information and how it is commissioned.
///
/// The passcode is a secret; its `Debug` output is redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeIdentity {
    /// The vendor id; `0xFFF1` to `0xFFF4` are the test vendor ids.
    pub vendor_id: VendorId,
    /// The product id.
    pub product_id: ProductId,
    /// The vendor name shown to controllers.
    pub vendor_name: String,
    /// The product name shown to controllers.
    pub product_name: String,
    /// The user-visible node label.
    pub node_label: String,
    /// The serial number, if the product has one.
    pub serial_number: Option<String>,
    /// The numeric software version.
    pub software_version: u32,
    /// The human-readable software version.
    pub software_version_string: String,
    /// A unique id of this node instance, stable across restarts.
    pub unique_id: String,
    /// The setup passcode used for commissioning.
    pub passcode: Secret<u32>,
    /// The 12-bit commissioning discriminator.
    pub discriminator: u16,
}

/// How the node reaches the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkConfig {
    /// The network interface to bind to, by name; `None` lets the engine choose.
    pub interface: Option<String>,
    /// Whether the node listens on IPv4.
    pub ipv4: bool,
    /// Whether the node listens on IPv6.
    pub ipv6: bool,
    /// The UDP port for Matter traffic.
    pub port: u16,
}

impl Default for NetworkConfig {
    /// Every interface, both IP families, the default Matter port.
    fn default() -> Self {
        Self {
            interface: None,
            ipv4: true,
            ipv6: true,
            port: MATTER_PORT,
        }
    }
}

/// The device attestation credentials the node presents when it is commissioned.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Attestation {
    /// The test credentials of the engine (test vendor id, test PAA). Not trusted by
    /// production ecosystems, accepted by development ones.
    Test,
    /// Credentials loaded from files by the product: the certification declaration, the
    /// product attestation intermediate certificate, the device attestation certificate and its
    /// private key, each as the bytes of its file.
    Files {
        /// The certification declaration.
        cd: Vec<u8>,
        /// The product attestation intermediate certificate.
        pai: Vec<u8>,
        /// The device attestation certificate.
        dac: Vec<u8>,
        /// The private key of the device attestation certificate.
        dac_key: Secret<Vec<u8>>,
    },
}

/// The role the node plays; it decides which endpoints the engine creates on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeRole {
    /// A bridge: the engine creates the root endpoint and an aggregator endpoint with the
    /// utility clusters both need; the product adds bridged endpoints under the aggregator.
    Bridge {
        /// The id of the aggregator endpoint, chosen by the product like every other endpoint
        /// id. Bridged endpoints with `parent: None` hang under it.
        aggregator: EndpointId,
    },
}

#[cfg(test)]
mod tests {
    use alloc::format;
    use alloc::string::String;

    use super::{
        Attestation, INVALID_PASSCODES, MATTER_PORT, MAX_PASSCODE, NetworkConfig, NodeIdentity,
        is_valid_passcode,
    };
    use crate::id::{ProductId, VendorId};
    use crate::secret::{REDACTED, Secret};

    #[test]
    fn passcode_validity_follows_the_specification() {
        assert!(is_valid_passcode(1));
        assert!(is_valid_passcode(20_202_021));
        assert!(is_valid_passcode(MAX_PASSCODE));
        assert!(!is_valid_passcode(0));
        assert!(!is_valid_passcode(MAX_PASSCODE + 1));
        for invalid in INVALID_PASSCODES {
            assert!(!is_valid_passcode(invalid), "{invalid} must be invalid");
        }
    }

    #[test]
    fn network_config_default_is_the_matter_port_on_every_interface() {
        let config = NetworkConfig::default();
        assert_eq!(config.interface, None);
        assert!(config.ipv4);
        assert!(config.ipv6);
        assert_eq!(config.port, MATTER_PORT);
    }

    #[test]
    fn identity_debug_redacts_the_passcode() {
        let identity = NodeIdentity {
            vendor_id: VendorId::new(0xFFF1),
            product_id: ProductId::new(0x8001),
            vendor_name: String::from("vendor"),
            product_name: String::from("product"),
            node_label: String::from("label"),
            serial_number: None,
            software_version: 1,
            software_version_string: String::from("1.0"),
            unique_id: String::from("unique"),
            passcode: Secret::new(20_202_021),
            discriminator: 3840,
        };
        let output = format!("{identity:?}");
        assert!(output.contains(REDACTED));
        assert!(!output.contains("20202021"));
        assert!(!output.contains("20_202_021"));
    }

    #[test]
    fn attestation_files_redact_the_key() {
        let attestation = Attestation::Files {
            cd: alloc::vec![1],
            pai: alloc::vec![2],
            dac: alloc::vec![3],
            dac_key: Secret::new(alloc::vec![0xAB, 0xCD]),
        };
        let output = format!("{attestation:?}");
        assert!(output.contains(REDACTED));
        assert!(!output.contains("171"), "{output}");
    }
}
