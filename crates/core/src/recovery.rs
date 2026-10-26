//! The recovery key: 32 random bytes, shown once, the only way back into an account after a
//! forgotten password. See `docs/crypto.md#recovery-key`.

use std::fmt;

use rand_core::CryptoRng;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{
    Error, Result, encoding,
    kdf::hkdf32,
    keys::{RecoveryAuthKey, RecoveryWrapKey, os_rng, random_array},
};

const CHECKSUM_LEN: usize = 3;
const GROUP_LEN: usize = 4;

#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct RecoveryKey([u8; 32]);

impl RecoveryKey {
    pub fn generate() -> Self {
        Self::generate_with_rng(&mut os_rng())
    }

    pub(crate) fn generate_with_rng(rng: &mut impl CryptoRng) -> Self {
        Self(random_array(rng))
    }

    /// 56 Crockford Base32 characters in 14 groups of 4, e.g. `7K2M-…`.
    pub fn to_display(&self) -> Zeroizing<String> {
        let mut raw = Zeroizing::new(Vec::with_capacity(32 + CHECKSUM_LEN));
        raw.extend_from_slice(&self.0);
        raw.extend_from_slice(&checksum(&self.0));
        let encoded = Zeroizing::new(encoding::crockford_encode(&raw));
        let groups: Vec<&str> = encoded
            .as_bytes()
            .chunks(GROUP_LEN)
            .map(|g| std::str::from_utf8(g).expect("Crockford output is ASCII"))
            .collect();
        Zeroizing::new(groups.join("-"))
    }

    /// Accepts what [`Self::to_display`] produces, case-insensitively, with any spacing.
    pub fn parse(input: &str) -> Result<Self> {
        let raw = Zeroizing::new(encoding::crockford_decode(input).ok_or(Error::RecoveryKeyInvalid)?);
        if raw.len() != 32 + CHECKSUM_LEN {
            return Err(Error::RecoveryKeyInvalid);
        }
        let (key, sum) = raw.split_at(32);
        let key: [u8; 32] = key.try_into().expect("split at 32");
        if checksum(&key) != sum {
            return Err(Error::RecoveryKeyChecksum);
        }
        Ok(Self(key))
    }

    pub fn auth_key(&self) -> RecoveryAuthKey {
        RecoveryAuthKey::from_bytes(&*hkdf32(&self.0, b"memo/v1/recovery-auth")).expect("32 bytes")
    }

    pub(crate) fn wrap_key(&self) -> RecoveryWrapKey {
        RecoveryWrapKey::from_bytes(&*hkdf32(&self.0, b"memo/v1/recovery-wrap")).expect("32 bytes")
    }

    #[cfg(test)]
    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RecoveryKey(<redacted>)")
    }
}

fn checksum(key: &[u8; 32]) -> [u8; CHECKSUM_LEN] {
    let digest = Sha256::new().chain_update(b"memo/v1/recovery-checksum\0").chain_update(key).finalize();
    digest[..CHECKSUM_LEN].try_into().expect("digest is 32 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_format_and_roundtrip() {
        let key = RecoveryKey::generate();
        let shown = key.to_display();
        assert_eq!(shown.len(), 56 + 13);
        assert_eq!(shown.split('-').count(), 14);
        assert_eq!(RecoveryKey::parse(&shown).unwrap().as_bytes(), key.as_bytes());
        let sloppy = shown.to_lowercase().replace('-', " ").replace('0', "o").replace('1', "l");
        assert_eq!(RecoveryKey::parse(&sloppy).unwrap().as_bytes(), key.as_bytes());
    }

    #[test]
    fn typo_is_reported_as_checksum_error() {
        let shown = RecoveryKey::generate().to_display();
        let first = shown.chars().next().unwrap();
        let replacement = if first == 'A' { 'B' } else { 'A' };
        let typo: String = std::iter::once(replacement).chain(shown.chars().skip(1)).collect();
        assert_eq!(RecoveryKey::parse(&typo).unwrap_err(), Error::RecoveryKeyChecksum);
        assert_eq!(RecoveryKey::parse(&shown[..20]).unwrap_err(), Error::RecoveryKeyInvalid);
        assert_eq!(RecoveryKey::parse("not a key!").unwrap_err(), Error::RecoveryKeyInvalid);
    }
}
