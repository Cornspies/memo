//! The symmetric envelope used for every ciphertext. See `docs/crypto.md#symmetric-envelope`.
//!
//! ```text
//! envelope = 0x01 || nonce:[24] || ciphertext || tag:[16]
//! aad      = 0x01 || label || 0x00 || context
//! ```

use chacha20poly1305::{
    Key, KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rand_core::CryptoRng;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Error, Result, keys::random_array};

const FORMAT_V1: u8 = 0x01;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
pub const OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentKind {
    Original = 0,
    Thumbnail = 1,
}

/// What a ciphertext is for. Becomes the AAD, so a ciphertext only opens in its own context.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Purpose {
    AccountKey { user_id: Uuid },
    AccountKeyRecovery { user_id: Uuid },
    PrivateKey { user_id: Uuid },
    Vault { user_id: Uuid, version: u64 },
    Memo { memo_id: Uuid, version: u64, key_version: u32 },
    Attachment { attachment_id: Uuid, kind: AttachmentKind },
}

impl Purpose {
    fn aad(&self) -> Vec<u8> {
        let (label, context): (&[u8], Vec<u8>) = match *self {
            Self::AccountKey { user_id } => (b"memo/v1/account-key", user_id.as_bytes().to_vec()),
            Self::AccountKeyRecovery { user_id } => (b"memo/v1/account-key-recovery", user_id.as_bytes().to_vec()),
            Self::PrivateKey { user_id } => (b"memo/v1/private-key", user_id.as_bytes().to_vec()),
            Self::Vault { user_id, version } => {
                (b"memo/v1/vault", [user_id.as_bytes().as_slice(), &version.to_be_bytes()].concat())
            }
            Self::Memo { memo_id, version, key_version } => (
                b"memo/v1/memo",
                [memo_id.as_bytes().as_slice(), &version.to_be_bytes(), &key_version.to_be_bytes()].concat(),
            ),
            Self::Attachment { attachment_id, kind } => {
                (b"memo/v1/attachment", [attachment_id.as_bytes().as_slice(), &[kind as u8]].concat())
            }
        };
        [&[FORMAT_V1], label, &[0x00], &context].concat()
    }
}

pub(crate) fn seal(key: &[u8; 32], purpose: Purpose, plaintext: &[u8], rng: &mut impl CryptoRng) -> Vec<u8> {
    seal_with_nonce(key, purpose, plaintext, &random_array(rng))
}

fn seal_with_nonce(key: &[u8; 32], purpose: Purpose, plaintext: &[u8], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let aad = purpose.aad();
    let sealed = XChaCha20Poly1305::new(&Key::from(*key))
        .encrypt(&XNonce::from(*nonce), Payload { msg: plaintext, aad: &aad })
        .expect("XChaCha20-Poly1305 accepts any realistic message length");
    let mut envelope = Vec::with_capacity(1 + NONCE_LEN + sealed.len());
    envelope.push(FORMAT_V1);
    envelope.extend_from_slice(nonce);
    envelope.extend_from_slice(&sealed);
    envelope
}

pub(crate) fn open(key: &[u8; 32], purpose: Purpose, envelope: &[u8]) -> Result<Vec<u8>> {
    let (&format, rest) = envelope.split_first().ok_or(Error::Malformed("envelope"))?;
    if format != FORMAT_V1 {
        return Err(Error::UnsupportedFormat(format));
    }
    if rest.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::Malformed("envelope"));
    }
    let (nonce, sealed) = rest.split_at(NONCE_LEN);
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("split at nonce length");
    let aad = purpose.aad();
    XChaCha20Poly1305::new(&Key::from(*key))
        .decrypt(&XNonce::from(nonce), Payload { msg: sealed, aad: &aad })
        .map_err(|_| Error::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::os_rng;

    const KEY: [u8; 32] = [9; 32];

    fn memo(version: u64) -> Purpose {
        Purpose::Memo { memo_id: Uuid::from_bytes([1; 16]), version, key_version: 1 }
    }

    #[test]
    fn roundtrip_and_layout() {
        let envelope = seal(&KEY, memo(3), b"hello", &mut os_rng());
        assert_eq!(envelope[0], 0x01);
        assert_eq!(envelope.len(), OVERHEAD + 5);
        assert_eq!(open(&KEY, memo(3), &envelope).unwrap(), b"hello");
    }

    #[test]
    fn context_is_bound() {
        let envelope = seal(&KEY, memo(3), b"hello", &mut os_rng());
        assert_eq!(open(&KEY, memo(4), &envelope), Err(Error::Decrypt));
        let user_id = Uuid::from_bytes([1; 16]);
        assert_eq!(open(&KEY, Purpose::AccountKey { user_id }, &envelope), Err(Error::Decrypt));
        assert_eq!(open(&[8; 32], memo(3), &envelope), Err(Error::Decrypt));
    }

    #[test]
    fn rejects_tampering_and_garbage() {
        let mut envelope = seal(&KEY, memo(3), b"hello", &mut os_rng());
        *envelope.last_mut().unwrap() ^= 1;
        assert_eq!(open(&KEY, memo(3), &envelope), Err(Error::Decrypt));
        envelope[0] = 0x02;
        assert_eq!(open(&KEY, memo(3), &envelope), Err(Error::UnsupportedFormat(0x02)));
        assert_eq!(open(&KEY, memo(3), &[0x01, 0, 0]), Err(Error::Malformed("envelope")));
        assert_eq!(open(&KEY, memo(3), &[]), Err(Error::Malformed("envelope")));
    }

    #[test]
    fn aad_layout_matches_spec() {
        let aad =
            Purpose::Attachment { attachment_id: Uuid::from_bytes([0xaa; 16]), kind: AttachmentKind::Thumbnail }.aad();
        let mut expected = vec![0x01];
        expected.extend_from_slice(b"memo/v1/attachment\0");
        expected.extend_from_slice(&[0xaa; 16]);
        expected.push(1);
        assert_eq!(aad, expected);
    }
}
