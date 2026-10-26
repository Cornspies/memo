//! Wrapping memo keys for members with HPKE auth mode, and contact fingerprints.
//! See `docs/crypto.md#memo-keys-and-sharing`.

use hpke::{Deserializable, OpModeR, OpModeS, Serializable, aead::ChaCha20Poly1305, kdf::HkdfSha256};
use rand_core::CryptoRng;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    Error, Result,
    keys::{Kem, MemoKey, PublicKey, UserKeyPair, os_rng},
};

const FORMAT_V1: u8 = 0x01;
const ENC_LEN: usize = 32;
pub const WRAPPED_KEY_LEN: usize = 1 + ENC_LEN + 32 + 16;

/// Identifies one wrapped copy of a memo key. Bound into the HPKE `info`, so a wrapped key
/// cannot be replayed for another memo, key version, owner or member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyGrant {
    pub memo_id: Uuid,
    pub key_version: u32,
    pub owner_id: Uuid,
    pub member_id: Uuid,
}

impl KeyGrant {
    fn info(&self) -> Vec<u8> {
        [
            b"memo/v1/memo-key\0".as_slice(),
            self.memo_id.as_bytes(),
            &self.key_version.to_be_bytes(),
            self.owner_id.as_bytes(),
            self.member_id.as_bytes(),
        ]
        .concat()
    }
}

/// Run by the memo owner. `owner` must be the key pair of `grant.owner_id`.
pub fn wrap_memo_key(owner: &UserKeyPair, member_key: &PublicKey, grant: &KeyGrant, memo_key: &MemoKey) -> Vec<u8> {
    wrap_memo_key_with_rng(owner, member_key, grant, memo_key, &mut os_rng())
}

pub(crate) fn wrap_memo_key_with_rng(
    owner: &UserKeyPair,
    member_key: &PublicKey,
    grant: &KeyGrant,
    memo_key: &MemoKey,
    rng: &mut impl CryptoRng,
) -> Vec<u8> {
    let mode = OpModeS::<Kem>::Auth((owner.hpke_secret().clone(), owner.public_key().to_hpke()));
    let (enc, ct) = hpke::single_shot_seal_with_rng::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &mode,
        &member_key.to_hpke(),
        &grant.info(),
        memo_key.as_bytes(),
        &[],
        rng,
    )
    .expect("HPKE seal with a valid X25519 key cannot fail");
    let mut wrapped = Vec::with_capacity(WRAPPED_KEY_LEN);
    wrapped.push(FORMAT_V1);
    wrapped.extend_from_slice(&enc.to_bytes());
    wrapped.extend_from_slice(&ct);
    wrapped
}

/// Run by a member. `owner_key` must be the **pinned** public key of `grant.owner_id`; this is
/// what proves the memo key really came from the owner.
pub fn unwrap_memo_key(
    member: &UserKeyPair,
    owner_key: &PublicKey,
    grant: &KeyGrant,
    wrapped: &[u8],
) -> Result<MemoKey> {
    let (&format, rest) = wrapped.split_first().ok_or(Error::Malformed("wrapped key"))?;
    if format != FORMAT_V1 {
        return Err(Error::UnsupportedFormat(format));
    }
    if wrapped.len() != WRAPPED_KEY_LEN {
        return Err(Error::Malformed("wrapped key"));
    }
    let (enc, ct) = rest.split_at(ENC_LEN);
    let enc = <Kem as hpke::Kem>::EncappedKey::from_bytes(enc).map_err(|_| Error::Malformed("wrapped key"))?;
    let plaintext = Zeroizing::new(
        hpke::single_shot_open::<ChaCha20Poly1305, HkdfSha256, Kem>(
            &OpModeR::Auth(owner_key.to_hpke()),
            member.hpke_secret(),
            &enc,
            &grant.info(),
            ct,
            &[],
        )
        .map_err(|_| Error::Decrypt)?,
    );
    MemoKey::from_bytes(&plaintext)
}

/// 8 groups of 4 hex characters, compared out of band to verify a contact's key.
pub fn fingerprint(user_id: Uuid, public_key: &PublicKey) -> String {
    let digest = Sha256::new()
        .chain_update(b"memo/v1/fingerprint\0")
        .chain_update(user_id.as_bytes())
        .chain_update(public_key.as_bytes())
        .finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    hex.as_bytes().chunks(4).map(|g| std::str::from_utf8(g).expect("hex is ASCII")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Setup {
        owner: UserKeyPair,
        member: UserKeyPair,
        grant: KeyGrant,
        memo_key: MemoKey,
    }

    fn setup() -> Setup {
        Setup {
            owner: UserKeyPair::generate(),
            member: UserKeyPair::generate(),
            grant: KeyGrant {
                memo_id: Uuid::now_v7(),
                key_version: 2,
                owner_id: Uuid::now_v7(),
                member_id: Uuid::now_v7(),
            },
            memo_key: MemoKey::generate(),
        }
    }

    #[test]
    fn member_unwraps_owner_key() {
        let s = setup();
        let wrapped = wrap_memo_key(&s.owner, s.member.public_key(), &s.grant, &s.memo_key);
        assert_eq!(wrapped.len(), WRAPPED_KEY_LEN);
        let unwrapped = unwrap_memo_key(&s.member, s.owner.public_key(), &s.grant, &wrapped).unwrap();
        assert_eq!(unwrapped, s.memo_key);
    }

    #[test]
    fn owner_can_wrap_for_self() {
        let s = setup();
        let grant = KeyGrant { member_id: s.grant.owner_id, ..s.grant };
        let wrapped = wrap_memo_key(&s.owner, s.owner.public_key(), &grant, &s.memo_key);
        assert_eq!(unwrap_memo_key(&s.owner, s.owner.public_key(), &grant, &wrapped).unwrap(), s.memo_key);
    }

    #[test]
    fn forged_by_someone_else_is_rejected() {
        let s = setup();
        let impostor = UserKeyPair::generate();
        let forged = wrap_memo_key(&impostor, s.member.public_key(), &s.grant, &MemoKey::generate());
        assert_eq!(unwrap_memo_key(&s.member, s.owner.public_key(), &s.grant, &forged), Err(Error::Decrypt));
    }

    #[test]
    fn grant_context_is_bound() {
        let s = setup();
        let wrapped = wrap_memo_key(&s.owner, s.member.public_key(), &s.grant, &s.memo_key);
        for other in [
            KeyGrant { key_version: 3, ..s.grant },
            KeyGrant { memo_id: Uuid::now_v7(), ..s.grant },
            KeyGrant { member_id: Uuid::now_v7(), ..s.grant },
            KeyGrant { owner_id: Uuid::now_v7(), ..s.grant },
        ] {
            assert_eq!(unwrap_memo_key(&s.member, s.owner.public_key(), &other, &wrapped), Err(Error::Decrypt));
        }
    }

    #[test]
    fn fingerprint_format() {
        let fp = fingerprint(Uuid::nil(), UserKeyPair::generate().public_key());
        assert_eq!(fp.len(), 32 + 7);
        assert!(fp.split(' ').all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_hexdigit())));
    }
}
