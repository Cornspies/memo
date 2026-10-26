//! Password stretching (Argon2id) and HKDF derivations. See `docs/crypto.md#password-derived-keys`.

use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use rand_core::CryptoRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

use crate::{
    Error, Result,
    keys::{AuthKey, MasterKey, os_rng, random_array},
};

pub const MIN_PASSWORD_CHARS: usize = 12;
pub const SALT_LEN: usize = 16;

pub type Salt = [u8; SALT_LEN];

/// Argon2id cost parameters. `m` is in KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    pub const DEFAULT: Self = Self { m: 65536, t: 3, p: 1 };
    /// Anything weaker would let a malicious server brute-force passwords cheaply.
    pub const MIN: Self = Self { m: 65536, t: 3, p: 1 };
    /// Anything stronger could be used to make clients hang.
    pub const MAX: Self = Self { m: 1_048_576, t: 20, p: 8 };

    pub fn check_bounds(&self) -> Result<()> {
        let within = |v: u32, min: u32, max: u32| (min..=max).contains(&v);
        if within(self.m, Self::MIN.m, Self::MAX.m)
            && within(self.t, Self::MIN.t, Self::MAX.t)
            && within(self.p, Self::MIN.p, Self::MAX.p)
        {
            Ok(())
        } else {
            Err(Error::KdfParamsOutOfBounds)
        }
    }
}

impl Default for KdfParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

pub struct PasswordKeys {
    pub auth_key: AuthKey,
    pub master_key: MasterKey,
}

pub fn generate_salt() -> Salt {
    generate_salt_with_rng(&mut os_rng())
}

pub(crate) fn generate_salt_with_rng(rng: &mut impl CryptoRng) -> Salt {
    random_array(rng)
}

/// Applies only when a password is chosen (registration, change, recovery), never at login.
pub fn check_password_policy(password: &str) -> Result<()> {
    if password.nfc().count() < MIN_PASSWORD_CHARS {
        return Err(Error::PasswordTooShort);
    }
    Ok(())
}

/// Rejects parameters outside [`KdfParams::MIN`]..=[`KdfParams::MAX`] before doing any work.
pub fn derive_password_keys(password: &str, salt: &Salt, params: KdfParams) -> Result<PasswordKeys> {
    params.check_bounds()?;
    let normalized: Zeroizing<String> = Zeroizing::new(password.nfc().collect());
    let argon2_params = Params::new(params.m, params.t, params.p, Some(32)).map_err(|_| Error::KdfParamsOutOfBounds)?;
    let mut stretched = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, argon2_params)
        .hash_password_into(normalized.as_bytes(), salt, &mut *stretched)
        .map_err(|_| Error::KdfParamsOutOfBounds)?;
    Ok(PasswordKeys {
        auth_key: AuthKey::from_bytes(&*hkdf32(&*stretched, b"memo/v1/auth")).expect("32 bytes"),
        master_key: MasterKey::from_bytes(&*hkdf32(&*stretched, b"memo/v1/master")).expect("32 bytes"),
    })
}

/// HKDF-SHA256 with an empty salt, 32 bytes of output.
pub(crate) fn hkdf32(ikm: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut okm = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, ikm).expand(info, &mut *okm).expect("32 bytes is a valid HKDF-SHA256 length");
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds() {
        assert!(KdfParams::DEFAULT.check_bounds().is_ok());
        assert!(KdfParams::MAX.check_bounds().is_ok());
        for weak in [KdfParams { m: 65535, ..KdfParams::DEFAULT }, KdfParams { t: 2, ..KdfParams::DEFAULT }] {
            assert_eq!(weak.check_bounds(), Err(Error::KdfParamsOutOfBounds));
        }
        assert_eq!(KdfParams { p: 9, ..KdfParams::DEFAULT }.check_bounds(), Err(Error::KdfParamsOutOfBounds));
        assert!(derive_password_keys("whatever", &[0; 16], KdfParams { t: 1, ..KdfParams::DEFAULT }).is_err());
    }

    #[test]
    fn password_is_nfc_normalized() {
        let salt = [1u8; 16];
        let composed = derive_password_keys("caf\u{e9} au lait!", &salt, KdfParams::DEFAULT).unwrap();
        let decomposed = derive_password_keys("cafe\u{301} au lait!", &salt, KdfParams::DEFAULT).unwrap();
        assert_eq!(composed.auth_key, decomposed.auth_key);
        assert_eq!(composed.master_key, decomposed.master_key);
        assert_ne!(composed.auth_key.as_bytes(), composed.master_key.as_bytes());
    }

    #[test]
    fn password_policy_counts_characters() {
        assert_eq!(check_password_policy("short"), Err(Error::PasswordTooShort));
        assert!(check_password_policy("ünïcödé-chars").is_ok());
        assert_eq!(
            check_password_policy("e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}"),
            Err(Error::PasswordTooShort)
        );
    }
}
