//! An in-memory [`Store`]: the web client's store (online-only) and the one tests use.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::store::{AccessRecord, AttachmentRecord, MemoRecord, StateRecord, Store, SyncMeta, VaultRecord};
use crate::{Result, envelope::AttachmentKind};

#[derive(Clone, Debug, Default)]
struct Data {
    meta: SyncMeta,
    memos: BTreeMap<Uuid, MemoRecord>,
    access: BTreeMap<Uuid, AccessRecord>,
    states: BTreeMap<Uuid, StateRecord>,
    attachments: BTreeMap<Uuid, AttachmentRecord>,
    vault: Option<VaultRecord>,
    blobs: BTreeMap<(Uuid, AttachmentKind), Vec<u8>>,
}

/// Transactions snapshot the whole store on `begin`; fine for the sizes a browser tab holds.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    data: Data,
    snapshot: Option<Box<Data>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Store for MemoryStore {
    fn begin(&mut self) -> Result<()> {
        debug_assert!(self.snapshot.is_none(), "nested transaction");
        self.snapshot = Some(Box::new(self.data.clone()));
        Ok(())
    }

    fn commit(&mut self) -> Result<()> {
        self.snapshot = None;
        Ok(())
    }

    fn rollback(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            self.data = *snapshot;
        }
    }

    fn meta(&self) -> Result<SyncMeta> {
        Ok(self.data.meta.clone())
    }

    fn set_meta(&mut self, meta: &SyncMeta) -> Result<()> {
        self.data.meta = meta.clone();
        Ok(())
    }

    fn memo(&self, id: Uuid) -> Result<Option<MemoRecord>> {
        Ok(self.data.memos.get(&id).cloned())
    }

    fn memo_ids(&self) -> Result<Vec<Uuid>> {
        Ok(self.data.memos.keys().copied().collect())
    }

    fn put_memo(&mut self, memo: &MemoRecord) -> Result<()> {
        self.data.memos.insert(memo.id, memo.clone());
        Ok(())
    }

    fn delete_memo(&mut self, id: Uuid) -> Result<()> {
        self.data.memos.remove(&id);
        Ok(())
    }

    fn access(&self, memo_id: Uuid) -> Result<Option<AccessRecord>> {
        Ok(self.data.access.get(&memo_id).cloned())
    }

    fn put_access(&mut self, access: &AccessRecord) -> Result<()> {
        self.data.access.insert(access.memo_id, access.clone());
        Ok(())
    }

    fn delete_access(&mut self, memo_id: Uuid) -> Result<()> {
        self.data.access.remove(&memo_id);
        Ok(())
    }

    fn state(&self, memo_id: Uuid) -> Result<Option<StateRecord>> {
        Ok(self.data.states.get(&memo_id).cloned())
    }

    fn state_ids(&self) -> Result<Vec<Uuid>> {
        Ok(self.data.states.keys().copied().collect())
    }

    fn put_state(&mut self, state: &StateRecord) -> Result<()> {
        self.data.states.insert(state.memo_id, state.clone());
        Ok(())
    }

    fn delete_state(&mut self, memo_id: Uuid) -> Result<()> {
        self.data.states.remove(&memo_id);
        Ok(())
    }

    fn attachment(&self, id: Uuid) -> Result<Option<AttachmentRecord>> {
        Ok(self.data.attachments.get(&id).cloned())
    }

    fn attachment_ids(&self) -> Result<Vec<Uuid>> {
        Ok(self.data.attachments.keys().copied().collect())
    }

    fn put_attachment(&mut self, attachment: &AttachmentRecord) -> Result<()> {
        self.data.attachments.insert(attachment.id, attachment.clone());
        Ok(())
    }

    fn delete_attachment(&mut self, id: Uuid) -> Result<()> {
        self.data.attachments.remove(&id);
        self.data.blobs.retain(|(blob_id, _), _| *blob_id != id);
        Ok(())
    }

    fn vault(&self) -> Result<Option<VaultRecord>> {
        Ok(self.data.vault.clone())
    }

    fn put_vault(&mut self, vault: &VaultRecord) -> Result<()> {
        self.data.vault = Some(vault.clone());
        Ok(())
    }

    fn blob(&self, id: Uuid, kind: AttachmentKind) -> Result<Option<Vec<u8>>> {
        Ok(self.data.blobs.get(&(id, kind)).cloned())
    }

    fn put_blob(&mut self, id: Uuid, kind: AttachmentKind, envelope: &[u8]) -> Result<()> {
        self.data.blobs.insert((id, kind), envelope.to_vec());
        Ok(())
    }
}
