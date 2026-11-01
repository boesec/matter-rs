//! Pairing information: the QR code text and the manual pairing code of a node identity.
//!
//! Both come from rs-matter's own computation (the QR payload and the Verhoeff-checked manual
//! code); rs-matter's `print_standard_qr_*` helpers, which log the codes, are never called.

use matter_rs_engine::{MAX_DISCRIMINATOR, NodeIdentity, PairingInfo, Secret, is_valid_passcode};
use rs_matter::BasicCommData;
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::pairing::qr::{CommFlowType, QrPayload, no_optional_data};
use rs_matter::sc::pase::{Spake2pVerifierPassword, Spake2pVerifierPasswordRef};

use crate::Error;

/// The commissioning data rs-matter needs: the passcode as a SPAKE2+ verifier password and the
/// discriminator. The passcode is exposed here and nowhere else in this crate.
pub(crate) fn commissioning_data(identity: &NodeIdentity) -> BasicCommData {
    let passcode = identity.passcode.expose().to_le_bytes();
    BasicCommData {
        password: Spake2pVerifierPassword::new_from_ref(Spake2pVerifierPasswordRef::new(&passcode)),
        discriminator: identity.discriminator,
    }
}

/// Checks the identity the way the engine does before it starts a node.
///
/// # Errors
///
/// [`matter_rs_engine::Error::InvalidIdentity`] for a discriminator beyond 12 bits or a
/// passcode the specification forbids; the message never contains the passcode.
pub(crate) fn validate_identity(identity: &NodeIdentity) -> Result<(), matter_rs_engine::Error> {
    if identity.discriminator > MAX_DISCRIMINATOR {
        return Err(matter_rs_engine::Error::InvalidIdentity {
            reason: format!(
                "discriminator {} exceeds the 12-bit maximum {MAX_DISCRIMINATOR}",
                identity.discriminator
            ),
        });
    }
    if !is_valid_passcode(*identity.passcode.expose()) {
        return Err(matter_rs_engine::Error::InvalidIdentity {
            reason: String::from(
                "the passcode is outside 1..=99999998 or one of the forbidden passcodes",
            ),
        });
    }
    Ok(())
}

/// The pairing information of a node identity, without a running node.
///
/// The QR code text is the standard commissioning flow payload with IP discovery; the manual
/// code is the 11-digit code without vendor and product ids. Both are secrets: they encode the
/// passcode. The one place that shows them is the `pairing-info` command of the service binary.
///
/// # Errors
///
/// [`Error::Engine`] with [`matter_rs_engine::Error::InvalidIdentity`] for a discriminator
/// beyond 12 bits or an invalid passcode, [`Error::RsMatter`] when rs-matter cannot render the
/// payload.
pub fn pairing_info_for(identity: &NodeIdentity) -> Result<PairingInfo, Error> {
    validate_identity(identity)?;
    let commissioning = commissioning_data(identity);
    let manual_code = commissioning.compute_pairing_code();
    let serial_number = identity.serial_number.as_deref().unwrap_or("");
    let config = BasicInfoConfig {
        vid: identity.vendor_id.raw(),
        pid: identity.product_id.raw(),
        serial_no: serial_number,
        ..BasicInfoConfig::new()
    };
    let payload = QrPayload::new_from_basic_info(
        DiscoveryCapabilities::IP,
        CommFlowType::Standard,
        commissioning,
        &config,
        no_optional_data,
    );
    let mut buffer = [0_u8; 512];
    let (qr_text, _) = payload.as_str(&mut buffer)?;
    Ok(PairingInfo {
        qr_text: Secret::new(qr_text.to_owned()),
        manual_code: Secret::new(manual_code.as_str().to_owned()),
        discriminator: identity.discriminator,
    })
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::{NodeIdentity, ProductId, REDACTED, Secret, VendorId};

    use super::pairing_info_for;

    pub(crate) fn test_identity() -> NodeIdentity {
        NodeIdentity {
            vendor_id: VendorId::new(0xFFF1),
            product_id: ProductId::new(0x8001),
            vendor_name: "matter-rs".into(),
            product_name: "On/Off node".into(),
            node_label: "Test node".into(),
            serial_number: None,
            software_version: 1,
            software_version_string: "0.0.0".into(),
            unique_id: "onoff-node-test".into(),
            passcode: Secret::new(20_202_021),
            discriminator: 3840,
        }
    }

    #[test]
    fn pairing_info_matches_known_vector() {
        let info = pairing_info_for(&test_identity()).unwrap();
        assert_eq!(info.manual_code.expose(), "34970112332");
        assert!(info.qr_text.expose().starts_with("MT:"));
        assert_eq!(info.discriminator, 3840);
        // The debug output of the result shows neither code.
        let output = format!("{info:?}");
        assert!(output.contains(REDACTED));
        assert!(!output.contains("34970112332"));
        assert!(!output.contains("MT:"));
    }

    #[test]
    fn qr_text_is_the_standard_payload() {
        // The same identity as rs-matter's test device, whose QR text is known.
        let mut identity = test_identity();
        identity.serial_number = Some("123456789".into());
        let info = pairing_info_for(&identity).unwrap();
        assert_eq!(
            info.qr_text.expose(),
            "MT:-24J0AFN00KA064IJ3P0WISA0DK5N1K8SQ1RYCU1O0"
        );
        let without_serial = pairing_info_for(&test_identity()).unwrap();
        assert_ne!(without_serial.qr_text, info.qr_text);
        assert_eq!(without_serial.manual_code, info.manual_code);
    }

    #[test]
    fn invalid_identities_are_rejected_without_the_passcode() {
        let mut identity = test_identity();
        identity.discriminator = 0x1000;
        let error = pairing_info_for(&identity).unwrap_err();
        assert!(error.to_string().contains("engine error"), "{error}");

        let mut identity = test_identity();
        identity.passcode = Secret::new(12_345_678);
        let error = pairing_info_for(&identity).unwrap_err();
        let chain = format!(
            "{error} / {}",
            core::error::Error::source(&error)
                .map(ToString::to_string)
                .unwrap_or_default()
        );
        assert!(chain.contains("passcode"), "{chain}");
        assert!(!chain.contains("12345678"), "{chain}");
    }
}
