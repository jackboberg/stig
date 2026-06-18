pub mod cli;
pub mod codegen;
pub mod config;
pub mod db;
pub mod errors;
pub mod migrate;
pub mod schema;
pub mod snapshot;
pub(crate) mod sql;

/// Compute the lower-case hex-encoded SHA-256 digest of `bytes`.
///
/// Thin wrapper around `sha2` + `hex`. Lives here rather than in a dedicated
/// module because it is a single delegating expression with no local domain
/// modelling — any crate module that needs it imports from `crate`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    #[test]
    fn sha256_hex_nist_empty_vector() {
        // NIST FIPS 180-4 known vector — confirms sha2+hex wiring is correct.
        assert_eq!(
            super::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        );
    }
}
