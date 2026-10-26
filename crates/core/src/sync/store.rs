//! Local state as the sync engine sees it, and the storage trait each client implements.
//! See `docs/sync.md#client-local-state`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Result,
    api::{Member, Permission},
    envelope::AttachmentKind,
    keys::MemoKey,
    payload::{MemoPayload, VaultPayload},
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncMeta {
    pub cursor: u64,
    pub epoch: Option<String>,
    /// A full resync is in progress; the next session restarts it from cursor 0.
    pub resetting: bool,
    /// That resync came with a new epoch: the server was restored from a backup, so memos it does
    /// not return may only exist on this device.
    pub server_restored: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoRecord {
    pub id: Uuid,
    pub owner_id: Uuid,
    /// The version `base` belongs to; 0 for a memo that is not on the server yet.
    pub server_version: u64,
    pub key_version: u32,
    /// Decrypted payload at `server_version`: the merge base. `None` until first decrypted.
    pub base: Option<MemoPayload>,
    /// Unsynced edit. `Some` means the memo is dirty.
    pub local: Option<MemoPayload>,
    pub highest_seen_version: u64,
    pub pending_delete: bool,
    pub conflict: Option<Conflict>,
    /// A newer server version this client could not use yet. Retried when keys arrive and at the
    /// start of each sync. Blocks pushing.
    pub undecoded: Option<Undecoded>,
    /// The last push of `local` was rejected. Blocks pushing (see [`PushError`]).
    pub push_error: Option<PushError>,
    /// What `memo.put`s without a known result carried. Recognises our own write when a response
    /// was lost.
    pub in_flight: Option<InFlight>,
    pub created_at: i64,
    pub updated_at: i64,
    pub updated_by: Option<Uuid>,
}

impl MemoRecord {
    pub fn is_dirty(&self) -> bool {
        self.local.is_some() || self.pending_delete
    }

    /// What the UI shows and edits.
    pub fn current(&self) -> Option<&MemoPayload> {
        self.local.as_ref().or(self.base.as_ref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    pub theirs: MemoPayload,
    pub theirs_version: u64,
    pub theirs_key_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undecoded {
    pub version: u64,
    pub key_version: u32,
    #[serde(with = "crate::encoding::b64")]
    pub ciphertext: Vec<u8>,
    pub reason: UndecodedReason,
    /// For [`UndecodedReason::NewerFormat`]: a read-only view, if the payload still parses.
    pub view: Option<MemoPayload>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndecodedReason {
    /// No verified key for `key_version` yet.
    MissingKey,
    /// Decryption failed: tampered, or a key that did not come from the owner.
    Undecryptable,
    /// Written by a newer client. Never rewritten by this one.
    NewerFormat,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushError {
    /// Lost write permission. Cleared when it comes back.
    Forbidden,
    /// Cleared by the next edit.
    Invalid,
    /// Retried once per sync session.
    QuotaExceeded,
}

/// At most one of these can have landed: they all had the same `base_version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InFlight {
    pub version: u64,
    pub payloads: Vec<MemoPayload>,
}

impl InFlight {
    const MAX_PAYLOADS: usize = 8;

    pub(crate) fn record(previous: Option<InFlight>, version: u64, payload: MemoPayload) -> InFlight {
        let mut payloads = previous.filter(|p| p.version == version).map(|p| p.payloads).unwrap_or_default();
        if !payloads.contains(&payload) {
            payloads.push(payload);
        }
        if payloads.len() > Self::MAX_PAYLOADS {
            payloads.remove(0);
        }
        InFlight { version, payloads }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecord {
    pub memo_id: Uuid,
    pub owner_id: Uuid,
    pub permission: Permission,
    pub members: Vec<Member>,
    /// Verified memo keys by `key_version`.
    pub keys: BTreeMap<u32, MemoKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRecord {
    pub memo_id: Uuid,
    pub pinned: bool,
    pub archived: bool,
    pub dirty: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRecord {
    pub id: Uuid,
    pub memo_id: Uuid,
    /// The server has a record of it (an upload succeeded or a change arrived).
    pub on_server: bool,
    pub uploaded: bool,
    pub thumb_uploaded: bool,
    pub upload_original: bool,
    pub upload_thumbnail: bool,
    pub pending_delete: bool,
}

impl AttachmentRecord {
    pub(crate) fn pending_upload(&self, kind: AttachmentKind) -> bool {
        match kind {
            AttachmentKind::Original => self.upload_original,
            AttachmentKind::Thumbnail => self.upload_thumbnail,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultRecord {
    pub server_version: u64,
    pub base: VaultPayload,
    pub local: Option<VaultPayload>,
    /// Written by a newer client: not decoded, never pushed.
    pub newer_format: bool,
}

impl VaultRecord {
    pub fn current(&self) -> &VaultPayload {
        self.local.as_ref().unwrap_or(&self.base)
    }
}

/// Local persistence. Implementations: SQLite (desktop), SQLDelight via UniFFI (Android),
/// [`MemoryStore`](super::MemoryStore) (web, tests).
///
/// Reads inside a transaction must see that transaction's writes. The engine never nests
/// transactions. Methods with default implementations scan everything; override them when the
/// backend can index (e.g. a `dirty` column derived from [`MemoRecord::is_dirty`]).
pub trait Store {
    fn begin(&mut self) -> Result<()>;
    fn commit(&mut self) -> Result<()>;
    fn rollback(&mut self);

    fn meta(&self) -> Result<SyncMeta>;
    fn set_meta(&mut self, meta: &SyncMeta) -> Result<()>;

    fn memo(&self, id: Uuid) -> Result<Option<MemoRecord>>;
    fn memo_ids(&self) -> Result<Vec<Uuid>>;
    fn put_memo(&mut self, memo: &MemoRecord) -> Result<()>;
    fn delete_memo(&mut self, id: Uuid) -> Result<()>;

    fn access(&self, memo_id: Uuid) -> Result<Option<AccessRecord>>;
    fn put_access(&mut self, access: &AccessRecord) -> Result<()>;
    fn delete_access(&mut self, memo_id: Uuid) -> Result<()>;

    fn state(&self, memo_id: Uuid) -> Result<Option<StateRecord>>;
    fn state_ids(&self) -> Result<Vec<Uuid>>;
    fn put_state(&mut self, state: &StateRecord) -> Result<()>;
    fn delete_state(&mut self, memo_id: Uuid) -> Result<()>;

    fn attachment(&self, id: Uuid) -> Result<Option<AttachmentRecord>>;
    fn attachment_ids(&self) -> Result<Vec<Uuid>>;
    fn put_attachment(&mut self, attachment: &AttachmentRecord) -> Result<()>;
    /// Deletes the record and its cached blobs.
    fn delete_attachment(&mut self, id: Uuid) -> Result<()>;

    fn vault(&self) -> Result<Option<VaultRecord>>;
    fn put_vault(&mut self, vault: &VaultRecord) -> Result<()>;

    /// Encrypted attachment bytes (envelopes), as uploaded and downloaded.
    fn blob(&self, id: Uuid, kind: AttachmentKind) -> Result<Option<Vec<u8>>>;
    fn put_blob(&mut self, id: Uuid, kind: AttachmentKind, envelope: &[u8]) -> Result<()>;

    fn dirty_memo_ids(&self) -> Result<Vec<Uuid>> {
        filter_memos(self, MemoRecord::is_dirty)
    }

    fn undecoded_memo_ids(&self) -> Result<Vec<Uuid>> {
        filter_memos(self, |m| m.undecoded.is_some())
    }

    fn dirty_state_ids(&self) -> Result<Vec<Uuid>> {
        let mut ids = Vec::new();
        for id in self.state_ids()? {
            if self.state(id)?.is_some_and(|s| s.dirty) {
                ids.push(id);
            }
        }
        Ok(ids)
    }

    fn attachments_of(&self, memo_id: Uuid) -> Result<Vec<AttachmentRecord>> {
        filter_attachments(self, |a| a.memo_id == memo_id)
    }

    /// Attachments with a pending upload or delete.
    fn pending_attachments(&self) -> Result<Vec<AttachmentRecord>> {
        filter_attachments(self, |a| a.upload_original || a.upload_thumbnail || a.pending_delete)
    }
}

fn filter_memos<S: Store + ?Sized>(store: &S, keep: impl Fn(&MemoRecord) -> bool) -> Result<Vec<Uuid>> {
    let mut ids = Vec::new();
    for id in store.memo_ids()? {
        if store.memo(id)?.is_some_and(|m| keep(&m)) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn filter_attachments<S: Store + ?Sized>(
    store: &S,
    keep: impl Fn(&AttachmentRecord) -> bool,
) -> Result<Vec<AttachmentRecord>> {
    let mut found = Vec::new();
    for id in store.attachment_ids()? {
        if let Some(a) = store.attachment(id)?.filter(|a| keep(a)) {
            found.push(a);
        }
    }
    Ok(found)
}
