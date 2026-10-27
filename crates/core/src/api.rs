//! HTTP API types, shared by the server and all clients. See `docs/api.md` and `docs/sync.md`.
//! Binary fields are base64url without padding on the wire; here they are bytes.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    encoding::{b64, b64_array, b64_opt},
    kdf::{KdfParams, Salt},
    keys::{AuthKey, PublicKey, RecoveryAuthKey},
};

pub const API_VERSION: u32 = 1;
/// Maximum number of ops in one sync push.
pub const MAX_PUSH_OPS: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthenticated,
    InvalidCredentials,
    Forbidden,
    InviteInvalid,
    NotFound,
    Conflict,
    TooLarge,
    QuotaExceeded,
    IncompleteKeys,
    RateLimited,
    Internal,
    /// A code added by a newer server.
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Health {
    pub api: u32,
    pub version: String,
}

// ---- Auth ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfConfig {
    #[serde(with = "b64_array")]
    pub salt: Salt,
    #[serde(flatten)]
    pub params: KdfParams,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceKind {
    Web,
    Desktop,
    Android,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub kind: DeviceKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreloginRequest {
    pub username: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreloginResponse {
    pub kdf: KdfConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterRequest {
    pub invite_code: String,
    pub user_id: Uuid,
    pub username: String,
    pub kdf: KdfConfig,
    pub auth_key: AuthKey,
    pub recovery_auth_key: RecoveryAuthKey,
    pub public_key: PublicKey,
    #[serde(with = "b64")]
    pub enc_account_key: Vec<u8>,
    #[serde(with = "b64")]
    pub enc_account_key_recovery: Vec<u8>,
    #[serde(with = "b64")]
    pub enc_private_key: Vec<u8>,
    #[serde(with = "b64")]
    pub vault: Vec<u8>,
    pub device: DeviceInfo,
}

/// Response of register and recover/complete. `token` is absent for web devices (cookie instead).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionResponse {
    pub device_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub auth_key: AuthKey,
    pub device: DeviceInfo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginResponse {
    pub user_id: Uuid,
    pub device_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    pub public_key: PublicKey,
    #[serde(with = "b64")]
    pub enc_account_key: Vec<u8>,
    #[serde(with = "b64")]
    pub enc_private_key: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasswordChangeRequest {
    /// The current one.
    pub auth_key: AuthKey,
    pub new: NewPassword,
    pub revoke_other_devices: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewPassword {
    pub kdf: KdfConfig,
    pub auth_key: AuthKey,
    #[serde(with = "b64")]
    pub enc_account_key: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverRequest {
    pub username: String,
    pub recovery_auth_key: RecoveryAuthKey,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverResponse {
    pub user_id: Uuid,
    pub recovery_token: String,
    #[serde(with = "b64")]
    pub enc_account_key_recovery: Vec<u8>,
    pub public_key: PublicKey,
    #[serde(with = "b64")]
    pub enc_private_key: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverCompleteRequest {
    pub recovery_token: String,
    pub kdf: KdfConfig,
    pub auth_key: AuthKey,
    #[serde(with = "b64")]
    pub enc_account_key: Vec<u8>,
    pub recovery_auth_key: RecoveryAuthKey,
    #[serde(with = "b64")]
    pub enc_account_key_recovery: Vec<u8>,
    pub device: DeviceInfo,
}

// ---- Account and users ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub user_id: Uuid,
    pub username: String,
    pub role: Role,
    pub public_key: PublicKey,
    pub quota_bytes: u64,
    pub used_bytes: u64,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub kind: DeviceKind,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteAccountRequest {
    pub auth_key: AuthKey,
}

/// `GET /users/lookup` and `GET /users/{id}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInfo {
    pub user_id: Uuid,
    pub username: String,
    pub public_key: PublicKey,
}

// ---- Sync ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncRequest {
    pub cursor: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epoch: Option<String>,
    #[serde(default)]
    pub push: Vec<PushOp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum PushOp {
    #[serde(rename = "memo.put")]
    MemoPut {
        id: Uuid,
        /// 0 creates the memo.
        base_version: u64,
        key_version: u32,
        #[serde(with = "b64")]
        ciphertext: Vec<u8>,
        /// The owner's own wrapped `memo_key[1]`; only on create.
        #[serde(with = "b64_opt", default, skip_serializing_if = "Option::is_none")]
        wrapped_key: Option<Vec<u8>>,
    },
    #[serde(rename = "memo.delete")]
    MemoDelete { id: Uuid, base_version: u64 },
    #[serde(rename = "state.put")]
    StatePut { memo_id: Uuid, pinned: bool, archived: bool },
    #[serde(rename = "attachment.delete")]
    AttachmentDelete { id: Uuid },
    #[serde(rename = "vault.put")]
    VaultPut {
        base_version: u64,
        #[serde(with = "b64")]
        ciphertext: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncResponse {
    /// Empty when `reset` is true: the server then ignores the push.
    pub results: Vec<OpResult>,
    pub changes: Vec<Change>,
    pub cursor: u64,
    pub epoch: String,
    pub more: bool,
    pub reset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpResult {
    pub status: OpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<u64>,
}

impl OpResult {
    pub fn status(status: OpStatus) -> Self {
        Self { status, version: None, current_version: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpStatus {
    Ok,
    Conflict,
    Deleted,
    Forbidden,
    NotFound,
    Invalid,
    QuotaExceeded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Change {
    Memo(MemoChange),
    Access(AccessChange),
    Attachment(AttachmentChange),
    State(StateChange),
    Vault(VaultChange),
}

impl Change {
    pub fn seq(&self) -> u64 {
        match self {
            Change::Memo(c) => c.seq,
            Change::Access(c) => c.seq,
            Change::Attachment(c) => c.seq,
            Change::State(c) => c.seq,
            Change::Vault(c) => c.seq,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoChange {
    pub seq: u64,
    pub id: Uuid,
    pub owner_id: Uuid,
    pub version: u64,
    pub key_version: u32,
    /// Absent once deleted.
    #[serde(with = "b64_opt", default, skip_serializing_if = "Option::is_none")]
    pub ciphertext: Option<Vec<u8>>,
    pub deleted: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub updated_by: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrappedKey {
    pub key_version: u32,
    #[serde(with = "b64")]
    pub wrapped_key: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub user_id: Uuid,
    pub username: String,
    /// As the server claims it. Clients compare it with their pin, never trust it blindly.
    pub public_key: PublicKey,
    pub permission: Permission,
}

/// The caller's access to a memo. When `revoked` is true, every other field is empty.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessChange {
    pub seq: u64,
    pub memo_id: Uuid,
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<Uuid>,
    /// The caller's own permission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<Permission>,
    /// Only the keys wrapped for the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<WrappedKey>,
    /// Everyone with access, the owner included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentChange {
    pub seq: u64,
    pub id: Uuid,
    pub memo_id: Uuid,
    pub size: Option<u64>,
    pub thumb_size: Option<u64>,
    pub uploaded: bool,
    pub thumb_uploaded: bool,
    pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChange {
    pub seq: u64,
    pub memo_id: Uuid,
    pub pinned: bool,
    pub archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultChange {
    pub seq: u64,
    pub version: u64,
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
}

// ---- Sharing ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: Uuid,
    pub permission: Permission,
    /// Every existing key version.
    pub keys: Vec<WrappedKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateMemberRequest {
    pub permission: Permission,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeRequest {
    pub user_ids: Vec<Uuid>,
    pub base_version: u64,
    pub new_key_version: u32,
    /// The current payload as version `base_version + 1` under the new key.
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
    /// Exactly the remaining members, the owner included.
    pub keys: Vec<MemberKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberKey {
    pub user_id: Uuid,
    #[serde(with = "b64")]
    pub wrapped_key: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokeResponse {
    pub version: u64,
}

// ---- Revisions ----

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionInfo {
    pub version: u64,
    pub key_version: u32,
    pub updated_by: Option<Uuid>,
    pub updated_at: i64,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revision {
    pub version: u64,
    pub key_version: u32,
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn push_ops_match_the_spec() {
        let id: Uuid = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f".parse().unwrap();
        let ops = vec![
            PushOp::MemoPut { id, base_version: 0, key_version: 1, ciphertext: vec![1, 2], wrapped_key: Some(vec![3]) },
            PushOp::MemoPut { id, base_version: 4, key_version: 1, ciphertext: vec![1, 2], wrapped_key: None },
            PushOp::StatePut { memo_id: id, pinned: true, archived: false },
        ];
        assert_eq!(
            serde_json::to_value(&ops).unwrap(),
            json!([
                { "op": "memo.put", "id": id, "base_version": 0, "key_version": 1, "ciphertext": "AQI", "wrapped_key": "Aw" },
                { "op": "memo.put", "id": id, "base_version": 4, "key_version": 1, "ciphertext": "AQI" },
                { "op": "state.put", "memo_id": id, "pinned": true, "archived": false },
            ])
        );
        let back: Vec<PushOp> = serde_json::from_value(serde_json::to_value(&ops).unwrap()).unwrap();
        assert_eq!(back, ops);
    }

    #[test]
    fn changes_are_tagged_by_type() {
        let memo_id: Uuid = "0192f3c4-7a1b-7c2d-9e8f-0a1b2c3d4e5f".parse().unwrap();
        let revoked: Change = serde_json::from_value(json!({
            "type": "access", "seq": 1052, "memo_id": memo_id, "revoked": true
        }))
        .unwrap();
        assert_eq!(
            revoked,
            Change::Access(AccessChange {
                seq: 1052,
                memo_id,
                revoked: true,
                owner_id: None,
                permission: None,
                keys: vec![],
                members: vec![]
            })
        );
        assert_eq!(revoked.seq(), 1052);
    }

    #[test]
    fn kdf_config_is_flat_and_unknown_error_codes_parse() {
        let kdf = KdfConfig { salt: [7; 16], params: KdfParams::DEFAULT };
        let value = serde_json::to_value(kdf).unwrap();
        assert_eq!(value, json!({ "salt": "BwcHBwcHBwcHBwcHBwcHBw", "m": 65536, "t": 3, "p": 1 }));
        assert_eq!(serde_json::from_value::<KdfConfig>(value).unwrap(), kdf);
        let code: ErrorCode = serde_json::from_value(json!("brand_new_code")).unwrap();
        assert_eq!(code, ErrorCode::Unknown);
    }
}
