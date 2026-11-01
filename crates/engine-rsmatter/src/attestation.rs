//! Device attestation credentials: rs-matter's test set, or certificates and a key from files.

use matter_rs_engine::{Attestation, Error, Secret};
use rs_matter::crypto::{
    CanonPkcPublicKey, CanonPkcPublicKeyRef, CanonPkcSecretKey, CanonPkcSecretKeyRef,
};
use rs_matter::dm::clusters::dev_att::DeviceAttestation;
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TestDeviceAttestation};

/// The length of a raw P-256 private scalar.
const PRIVATE_KEY_LEN: usize = 32;

/// The length of an uncompressed P-256 point (`0x04` followed by X and Y).
const PUBLIC_KEY_LEN: usize = 65;

/// The attestation credentials a node runs with.
pub(crate) enum NodeAttestation {
    /// rs-matter's test credentials (vendor id `0xFFF1`, test PAA).
    Test(TestDeviceAttestation),
    /// Credentials from files.
    Files(FileAttestation),
}

impl NodeAttestation {
    /// Translates the facade's attestation choice.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidIdentity`] when the files cannot be used, see [`FileAttestation::new`].
    pub(crate) fn from_facade(attestation: Attestation) -> Result<Self, Error> {
        match attestation {
            Attestation::Test => Ok(Self::Test(TEST_DEV_ATT)),
            Attestation::Files {
                cd,
                pai,
                dac,
                dac_key,
            } => FileAttestation::new(cd, pai, dac, &dac_key).map(Self::Files),
            _ => Err(Error::Unsupported {
                operation: "this attestation kind",
            }),
        }
    }

    /// The credentials as rs-matter sees them.
    pub(crate) fn as_dyn(&self) -> &dyn DeviceAttestation {
        match self {
            Self::Test(test) => test,
            Self::Files(files) => files,
        }
    }

    /// The private key of the device attestation certificate, for the crypto back-end.
    pub(crate) fn private_key(&self) -> CanonPkcSecretKeyRef<'_> {
        match self {
            Self::Test(_) => DAC_PRIVKEY,
            Self::Files(files) => files.private_key.reference(),
        }
    }
}

/// Attestation credentials loaded from files by the product: the certification declaration,
/// the PAI and the DAC as DER, and the DAC's private key.
pub(crate) struct FileAttestation {
    cd: Vec<u8>,
    pai: Vec<u8>,
    dac: Vec<u8>,
    public_key: CanonPkcPublicKey,
    private_key: CanonPkcSecretKey,
}

impl FileAttestation {
    /// Builds the credentials.
    ///
    /// The key is accepted as the raw 32-byte P-256 scalar, or as DER (SEC1 `ECPrivateKey`,
    /// also when wrapped in PKCS#8), from which the scalar is taken. The public key is taken
    /// from the DAC's subject public key info. Nothing is logged.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidIdentity`] when the key or the certificate has none of the expected
    /// shapes; the message names the file, never its content.
    pub(crate) fn new(
        cd: Vec<u8>,
        pai: Vec<u8>,
        dac: Vec<u8>,
        dac_key: &Secret<Vec<u8>>,
    ) -> Result<Self, Error> {
        let scalar = private_scalar(dac_key.expose()).ok_or_else(|| Error::InvalidIdentity {
            reason: String::from(
                "the DAC private key is neither a raw 32-byte P-256 scalar nor a DER \
                 ECPrivateKey",
            ),
        })?;
        let point = public_point(&dac).ok_or_else(|| Error::InvalidIdentity {
            reason: String::from("the DAC does not carry an uncompressed P-256 public key"),
        })?;
        Ok(Self {
            cd,
            pai,
            dac,
            public_key: CanonPkcPublicKey::from(point),
            private_key: CanonPkcSecretKey::from(scalar),
        })
    }
}

impl DeviceAttestation for FileAttestation {
    fn cert_declaration(&self) -> &[u8] {
        &self.cd
    }

    fn pai(&self) -> &[u8] {
        &self.pai
    }

    fn dac(&self) -> &[u8] {
        &self.dac
    }

    fn dac_pub_key(&self) -> CanonPkcPublicKeyRef<'_> {
        self.public_key.reference()
    }

    fn dac_priv_key(&self) -> CanonPkcSecretKeyRef<'_> {
        self.private_key.reference()
    }
}

/// The P-256 private scalar of a key file: the raw 32 bytes, or the `privateKey` octet string
/// of a SEC1 `ECPrivateKey` (`version 1`, then `OCTET STRING` of 32 bytes), which PKCS#8 wraps
/// unchanged.
fn private_scalar(key: &[u8]) -> Option<[u8; PRIVATE_KEY_LEN]> {
    if let Ok(raw) = <[u8; PRIVATE_KEY_LEN]>::try_from(key) {
        return Some(raw);
    }
    // `INTEGER 1` (the ECPrivateKey version) followed by `OCTET STRING` of length 32.
    const MARKER: [u8; 5] = [0x02, 0x01, 0x01, 0x04, 0x20];
    let start = key.windows(MARKER.len()).position(|w| w == MARKER)? + MARKER.len();
    key.get(start..start + PRIVATE_KEY_LEN)
        .and_then(|scalar| scalar.try_into().ok())
}

/// The uncompressed P-256 public point of a DER certificate: the `BIT STRING` of the subject
/// public key info, which for P-256 is `03 42 00` followed by the 65-byte point.
fn public_point(certificate: &[u8]) -> Option<[u8; PUBLIC_KEY_LEN]> {
    const MARKER: [u8; 4] = [0x03, 0x42, 0x00, 0x04];
    let start = certificate
        .windows(MARKER.len())
        .position(|w| w == MARKER)?
        + MARKER.len()
        - 1;
    certificate
        .get(start..start + PUBLIC_KEY_LEN)
        .and_then(|point| point.try_into().ok())
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::{Attestation, Error, Secret};
    use rs_matter::dm::clusters::dev_att::DeviceAttestation;
    use rs_matter::dm::devices::test::TEST_DEV_ATT;

    use super::{NodeAttestation, private_scalar, public_point};

    #[test]
    fn test_credentials_are_rs_matters() {
        let attestation = NodeAttestation::from_facade(Attestation::Test).unwrap();
        assert_eq!(attestation.as_dyn().dac(), TEST_DEV_ATT.dac());
    }

    #[test]
    fn file_credentials_take_the_keys_from_the_test_set() {
        // The test DAC and a raw scalar reproduce rs-matter's own public key.
        let dac = TEST_DEV_ATT.dac().to_vec();
        let expected = TEST_DEV_ATT.dac_pub_key();
        let point = public_point(&dac).expect("the test DAC holds a P-256 key");
        assert_eq!(&point, expected.access());

        let mut der = vec![0x30, 0x77, 0x02, 0x01, 0x01, 0x04, 0x20];
        der.extend_from_slice(&[7_u8; 32]);
        der.extend_from_slice(&[0xA0, 0x0A]);
        assert_eq!(private_scalar(&der), Some([7_u8; 32]));
        assert_eq!(private_scalar(&[9_u8; 32]), Some([9_u8; 32]));
        assert_eq!(private_scalar(&[9_u8; 31]), None);

        let attestation = NodeAttestation::from_facade(Attestation::Files {
            cd: vec![1],
            pai: vec![2],
            dac: dac.clone(),
            dac_key: Secret::new(vec![7_u8; 32]),
        })
        .unwrap();
        assert_eq!(attestation.as_dyn().cert_declaration(), &[1]);
        assert_eq!(attestation.as_dyn().pai(), &[2]);
        assert_eq!(attestation.as_dyn().dac(), &dac[..]);
        assert_eq!(attestation.private_key().access(), &[7_u8; 32]);
    }

    #[test]
    fn unusable_files_are_rejected_without_their_content() {
        let Err(error) = NodeAttestation::from_facade(Attestation::Files {
            cd: vec![],
            pai: vec![],
            dac: vec![0xAB; 10],
            dac_key: Secret::new(vec![0xCD; 5]),
        }) else {
            panic!("unusable files were accepted");
        };
        match error {
            Error::InvalidIdentity { reason } => {
                assert!(reason.contains("private key"), "{reason}");
                assert!(!reason.contains("CD"), "{reason}");
            }
            other => panic!("unexpected error {other:?}"),
        }
    }
}
