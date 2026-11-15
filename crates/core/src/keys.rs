//! Key types. Every secret is a distinct type so one role's key cannot be passed where another
//! is expected, and all of them zeroize on drop and never print their contents.

use std::fmt;

use hpke::{Deserializable, Kem as _, Serializable};
use rand_core::CryptoRng;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{Error, Result, encoding};

pub(crate) type Kem = hpke::kem::X25519HkdfSha256;

pub(crate) fn os_rng() -> rand_core::UnwrapErr<getrandom::SysRng> {
    rand_core::UnwrapErr(getrandom::SysRng)
}

pub(crate) fn random_array<const N: usize>(rng: &mut impl CryptoRng) -> [u8; N] {
    let mut bytes = [0u8; N];
    rng.fill_bytes(&mut bytes);
    bytes
}

macro_rules! secret_key {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Zeroize, ZeroizeOnDrop)]
        pub struct $name([u8; 32]);

        impl $name {
            pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
                let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::Malformed("key length"))?;
                Ok(Self(bytes))
            }

            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            pub fn to_b64(&self) -> String {
                encoding::b64_encode(self.0)
            }

            #[allow(dead_code)]
            pub(crate) fn generate_with_rng(rng: &mut impl CryptoRng) -> Self {
                Self(random_array(rng))
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.0.ct_eq(&other.0).into()
            }
        }

        impl Eq for $name {}

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(<redacted>)"))
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(&self.to_b64())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                let text = Zeroizing::new(String::deserialize(d)?);
                let bytes = Zeroizing::new(encoding::b64_decode(&text).map_err(D::Error::custom)?);
                Self::from_bytes(&bytes).map_err(D::Error::custom)
            }
        }
    };
}

macro_rules! generate {
    ($($name:ident),*) => {
        $(impl $name {
            pub fn generate() -> Self {
                Self::generate_with_rng(&mut os_rng())
            }
        })*
    };
}

secret_key!(
    /// Sent to the server as the login credential. Derived from the password.
    AuthKey
);
secret_key!(
    /// Wraps the account key. Derived from the password, never leaves the device.
    MasterKey
);
secret_key!(
    /// Sent to the server to prove possession of the recovery key.
    RecoveryAuthKey
);
secret_key!(
    /// Wraps the recovery copy of the account key.
    RecoveryWrapKey
);
secret_key!(
    /// Random per user, permanent. Protects the private key and the vault.
    AccountKey
);
secret_key!(
    /// Random per memo and `key_version`.
    MemoKey
);
secret_key!(
    /// Random per attachment; stored inside the memo payload.
    AttachmentKey
);

generate!(AccountKey, MemoKey, AttachmentKey);

/// A user's X25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; 32]);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| Error::Malformed("public key length"))?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn to_hpke(self) -> <Kem as hpke::Kem>::PublicKey {
        <Kem as hpke::Kem>::PublicKey::from_bytes(&self.0).expect("32 bytes is a valid X25519 public key")
    }

    /// False for a low-order point: no key can be wrapped to it (the key agreement yields zero).
    pub fn is_usable(&self) -> bool {
        Kem::encap_with_rng(&self.to_hpke(), None, &mut os_rng()).is_ok()
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", encoding::b64_encode(self.0))
    }
}

impl Serialize for PublicKey {
    fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.serialize_str(&encoding::b64_encode(self.0))
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let bytes = encoding::b64_decode(&String::deserialize(d)?).map_err(D::Error::custom)?;
        Self::from_bytes(&bytes).map_err(D::Error::custom)
    }
}

/// A user's X25519 key pair. The private half zeroizes on drop (inside `x25519-dalek`).
#[derive(Clone)]
pub struct UserKeyPair {
    secret: <Kem as hpke::Kem>::PrivateKey,
    public: PublicKey,
}

impl UserKeyPair {
    pub fn generate() -> Self {
        Self::generate_with_rng(&mut os_rng())
    }

    pub(crate) fn generate_with_rng(rng: &mut impl CryptoRng) -> Self {
        let (secret, public) = Kem::gen_keypair_with_rng(rng);
        let public = PublicKey::from_bytes(&public.to_bytes()).expect("X25519 public keys are 32 bytes");
        Self { secret, public }
    }

    /// Rebuilds a key pair from its private half, recomputing the public key.
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self> {
        let secret =
            <Kem as hpke::Kem>::PrivateKey::from_bytes(bytes).map_err(|_| Error::Malformed("private key length"))?;
        let public =
            PublicKey::from_bytes(&Kem::sk_to_pk(&secret).to_bytes()).expect("X25519 public keys are 32 bytes");
        Ok(Self { secret, public })
    }

    pub fn secret_bytes(&self) -> Zeroizing<[u8; 32]> {
        let mut bytes = Zeroizing::new([0u8; 32]);
        bytes.copy_from_slice(&self.secret.to_bytes());
        bytes
    }

    pub fn public_key(&self) -> &PublicKey {
        &self.public
    }

    pub(crate) fn hpke_secret(&self) -> &<Kem as hpke::Kem>::PrivateKey {
        &self.secret
    }
}

impl fmt::Debug for UserKeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserKeyPair").field("public", &self.public).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_roundtrips_through_secret_bytes() {
        let pair = UserKeyPair::generate();
        let restored = UserKeyPair::from_secret_bytes(&*pair.secret_bytes()).unwrap();
        assert_eq!(restored.public_key(), pair.public_key());
    }

    /// Accounts registered before 0.2.2 hold the private key as generated, not clamped
    /// (docs/crypto.md, User key pair). RFC 7748, section 6.1: Alice's private key is not clamped.
    #[test]
    fn unclamped_secret_is_the_same_key() {
        let hex = |s: &str| (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
        let secret: Vec<u8> = hex("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let public: Vec<u8> = hex("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let pair = UserKeyPair::from_secret_bytes(&secret).unwrap();
        assert_eq!(pair.public_key().as_bytes().as_slice(), public);
        let clamped = pair.secret_bytes();
        assert_ne!(clamped.as_slice(), secret);
        assert_eq!(UserKeyPair::from_secret_bytes(&*clamped).unwrap().public_key(), pair.public_key());
    }

    #[test]
    fn debug_never_prints_secrets() {
        let key = MemoKey::from_bytes(&[7u8; 32]).unwrap();
        assert_eq!(format!("{key:?}"), "MemoKey(<redacted>)");
        assert!(!format!("{:?}", UserKeyPair::generate()).contains("secret"));
    }
}
