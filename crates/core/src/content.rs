//! Encrypting and decrypting memos, attachments and the vault.

use rand_core::CryptoRng;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    Error, Result,
    envelope::{self, AttachmentKind, Purpose},
    keys::{AccountKey, AttachmentKey, MemoKey, os_rng},
    payload::{self, MemoPayload, VaultPayload},
};

/// Where a memo ciphertext belongs. `version` is the version this ciphertext *is* (for an edit:
/// `base_version + 1`), so the server cannot present it as any other version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoContext {
    pub memo_id: Uuid,
    pub version: u64,
    pub key_version: u32,
}

impl MemoContext {
    fn purpose(&self) -> Result<Purpose> {
        if self.version == 0 || self.key_version == 0 {
            return Err(Error::Malformed("memo context"));
        }
        Ok(Purpose::Memo { memo_id: self.memo_id, version: self.version, key_version: self.key_version })
    }
}

pub fn seal_memo(key: &MemoKey, ctx: &MemoContext, payload: &MemoPayload) -> Result<Vec<u8>> {
    seal_memo_with_rng(key, ctx, payload, &mut os_rng())
}

pub(crate) fn seal_memo_with_rng(
    key: &MemoKey,
    ctx: &MemoContext,
    payload: &MemoPayload,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>> {
    let plaintext = payload::encode_memo(payload)?;
    Ok(envelope::seal(key.as_bytes(), ctx.purpose()?, &plaintext, rng))
}

pub fn open_memo(key: &MemoKey, ctx: &MemoContext, ciphertext: &[u8]) -> Result<MemoPayload> {
    let plaintext = Zeroizing::new(envelope::open(key.as_bytes(), ctx.purpose()?, ciphertext)?);
    payload::decode_memo(&plaintext)
}

pub fn seal_attachment(key: &AttachmentKey, attachment_id: Uuid, kind: AttachmentKind, bytes: &[u8]) -> Vec<u8> {
    seal_attachment_with_rng(key, attachment_id, kind, bytes, &mut os_rng())
}

pub(crate) fn seal_attachment_with_rng(
    key: &AttachmentKey,
    attachment_id: Uuid,
    kind: AttachmentKind,
    bytes: &[u8],
    rng: &mut impl CryptoRng,
) -> Vec<u8> {
    envelope::seal(key.as_bytes(), Purpose::Attachment { attachment_id, kind }, bytes, rng)
}

pub fn open_attachment(
    key: &AttachmentKey,
    attachment_id: Uuid,
    kind: AttachmentKind,
    ciphertext: &[u8],
) -> Result<Vec<u8>> {
    envelope::open(key.as_bytes(), Purpose::Attachment { attachment_id, kind }, ciphertext)
}

/// `version` is the version this ciphertext is: 1 at registration, `base_version + 1` after.
pub fn seal_vault(key: &AccountKey, user_id: Uuid, version: u64, vault: &VaultPayload) -> Result<Vec<u8>> {
    seal_vault_with_rng(key, user_id, version, vault, &mut os_rng())
}

pub(crate) fn seal_vault_with_rng(
    key: &AccountKey,
    user_id: Uuid,
    version: u64,
    vault: &VaultPayload,
    rng: &mut impl CryptoRng,
) -> Result<Vec<u8>> {
    let plaintext = payload::encode_vault(vault)?;
    Ok(envelope::seal(key.as_bytes(), Purpose::Vault { user_id, version }, &plaintext, rng))
}

pub fn open_vault(key: &AccountKey, user_id: Uuid, version: u64, ciphertext: &[u8]) -> Result<VaultPayload> {
    let plaintext = Zeroizing::new(envelope::open(key.as_bytes(), Purpose::Vault { user_id, version }, ciphertext)?);
    payload::decode_vault(&plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memo_roundtrip_and_version_binding() {
        let key = MemoKey::generate();
        let ctx = MemoContext { memo_id: Uuid::now_v7(), version: 5, key_version: 1 };
        let payload = MemoPayload::new("hello #world", 42);
        let ciphertext = seal_memo(&key, &ctx, &payload).unwrap();
        assert_eq!(open_memo(&key, &ctx, &ciphertext).unwrap(), payload);
        let replayed = MemoContext { version: 6, ..ctx };
        assert_eq!(open_memo(&key, &replayed, &ciphertext), Err(Error::Decrypt));
    }

    #[test]
    fn memo_versions_start_at_one() {
        let ctx = MemoContext { memo_id: Uuid::now_v7(), version: 0, key_version: 1 };
        assert!(seal_memo(&MemoKey::generate(), &ctx, &MemoPayload::new("", 0)).is_err());
    }

    #[test]
    fn attachment_kinds_are_not_interchangeable() {
        let key = AttachmentKey::generate();
        let id = Uuid::now_v7();
        let original = seal_attachment(&key, id, AttachmentKind::Original, b"\xff\xd8\xff jpeg bytes");
        assert_eq!(open_attachment(&key, id, AttachmentKind::Original, &original).unwrap(), b"\xff\xd8\xff jpeg bytes");
        assert_eq!(open_attachment(&key, id, AttachmentKind::Thumbnail, &original), Err(Error::Decrypt));
    }

    #[test]
    fn vault_roundtrip() {
        let key = AccountKey::generate();
        let user_id = Uuid::now_v7();
        let vault = VaultPayload::default();
        let ciphertext = seal_vault(&key, user_id, 1, &vault).unwrap();
        assert_eq!(open_vault(&key, user_id, 1, &ciphertext).unwrap(), vault);
        assert_eq!(open_vault(&key, user_id, 2, &ciphertext), Err(Error::Decrypt));
    }
}
