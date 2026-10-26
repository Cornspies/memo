use crate::kdf::MIN_PASSWORD_CHARS;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    /// Wrong key, tampered ciphertext or mismatched context. Deliberately carries no detail.
    #[error("decryption failed")]
    Decrypt,
    #[error("unsupported envelope format {0:#04x}")]
    UnsupportedFormat(u8),
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("KDF parameters outside the accepted bounds")]
    KdfParamsOutOfBounds,
    #[error("password must be at least {MIN_PASSWORD_CHARS} characters")]
    PasswordTooShort,
    #[error("recovery key is not valid")]
    RecoveryKeyInvalid,
    #[error("recovery key checksum mismatch (typo?)")]
    RecoveryKeyChecksum,
    #[error("private key does not match the public key")]
    KeyMismatch,
    #[error("payload version {0} is not supported by this client")]
    UnsupportedPayloadVersion(u32),
    #[error("not found")]
    NotFound,
    #[error("only the owner can do this")]
    NotOwner,
    #[error("read-only")]
    ReadOnly,
    #[error("the memo has an unresolved conflict")]
    InConflict,
    /// The server presented a different public key than the one pinned for this user.
    #[error("public key of user {0} changed")]
    KeyChanged(uuid::Uuid),
    #[error("no pinned public key for user {0}")]
    NotPinned(uuid::Uuid),
    /// Returned by [`crate::Store`] implementations.
    #[error("local store: {0}")]
    Store(String),
}
