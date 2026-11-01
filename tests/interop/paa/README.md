# Test PAA certificates

Product Attestation Authority roots that the Python test framework trusts when it commissions a
device under test. They are mounted read-only into the framework's container as `/paa` and passed
with `--paa-trust-store-path /paa`; without an explicit store the framework looks next to its own
scripts, finds nothing and fails device attestation.

Both files are the official Matter **test** roots, taken unchanged from the connectedhomeip
repository (Apache-2.0), directory `credentials/development/paa-root-certs/`:

| File | Used by |
|---|---|
| `Chip-Test-PAA-NoVID-Cert.der` | matter.js devices (their default attestation chain) |
| `Chip-Test-PAA-FFF1-Cert.der` | rs-matter's test credentials for the test vendor id `0xFFF1` |

Source: <https://github.com/project-chip/connectedhomeip/tree/master/credentials/development/paa-root-certs>.
These roots are for development and testing only; no production device chains to them.
