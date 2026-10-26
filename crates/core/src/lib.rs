//! Shared types, crypto, merge and sync logic for all memo clients and the server.
//! See `docs/` for the specification this crate implements; `docs/crypto.md` in particular.

mod account;
pub mod api;
mod content;
mod encoding;
mod envelope;
mod error;
mod kdf;
mod keys;
mod merge;
mod payload;
mod recovery;
mod share;
pub mod sync;
#[cfg(test)]
mod vectors;

pub use account::{
    PasswordChange, RecoveryRotation, Registration, UnlockedAccount, recover_account_key, register, unlock,
    unlock_with_account_key,
};
pub use content::{MemoContext, open_attachment, open_memo, open_vault, seal_attachment, seal_memo, seal_vault};
pub use encoding::{b64_decode, b64_encode};
pub use envelope::{AttachmentKind, OVERHEAD as ENVELOPE_OVERHEAD};
pub use error::{Error, Result};
pub use kdf::{
    KdfParams, MIN_PASSWORD_CHARS, PasswordKeys, SALT_LEN, Salt, check_password_policy, derive_password_keys,
    generate_salt,
};
pub use keys::{AccountKey, AttachmentKey, AuthKey, MasterKey, MemoKey, PublicKey, RecoveryAuthKey, UserKeyPair};
pub use merge::{MemoMerge, PinConflict, merge_memo, merge_vault};
pub use payload::{
    AttachmentRef, Contact, MEMO_PAYLOAD_VERSION, MemoPayload, ThumbRef, VAULT_PAYLOAD_VERSION, VaultPayload,
};
pub use recovery::RecoveryKey;
pub use share::{KeyGrant, WRAPPED_KEY_LEN, fingerprint, unwrap_memo_key, wrap_memo_key};
pub use uuid::Uuid;
