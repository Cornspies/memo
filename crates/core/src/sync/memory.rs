//! An in-memory [`Store`]: the web client's store (online-only) and the one tests use.

use std::{cell::Cell, collections::BTreeMap, sync::Arc};

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
    blobs: BTreeMap<(Uuid, AttachmentKind), Blob>,
}

/// An envelope, shared so that a snapshot does not copy the images, and when it was last read.
#[derive(Clone, Debug)]
struct Blob {
    envelope: Arc<[u8]>,
    used: Cell<u64>,
}

/// Transactions snapshot the records on `begin`; fine for the sizes a browser tab holds.
#[derive(Clone, Debug, Default)]
pub struct MemoryStore {
    data: Data,
    snapshot: Option<Box<Data>>,
    generation: u64,
    /// Counts blob reads and writes, to order them by last use.
    clock: Cell<u64>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Evicts cached blobs, least recently used first, until they take at most `budget` bytes.
    /// Blobs of attachments with a pending upload stay and do not count. Returns the bytes freed.
    pub fn trim_blobs(&mut self, budget: u64) -> u64 {
        let attachments = &self.data.attachments;
        let uploading = |id: &Uuid| attachments.get(id).is_some_and(|a| a.upload_original || a.upload_thumbnail);
        let mut cached: Vec<_> = self
            .data
            .blobs
            .iter()
            .filter(|((id, _), _)| !uploading(id))
            .map(|(key, blob)| (blob.used.get(), *key, blob.envelope.len() as u64))
            .collect();
        cached.sort_unstable_by_key(|(used, ..)| std::cmp::Reverse(*used));
        let (mut kept, mut freed) = (0, 0);
        for (_, key, size) in cached {
            if kept + size <= budget {
                kept += size;
            } else {
                self.data.blobs.remove(&key);
                freed += size;
            }
        }
        freed
    }

    fn tick(&self) -> u64 {
        self.clock.set(self.clock.get() + 1);
        self.clock.get()
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
            self.generation += 1;
        }
    }

    fn generation(&self) -> u64 {
        self.generation
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
        self.generation += 1;
        Ok(())
    }

    fn delete_memo(&mut self, id: Uuid) -> Result<()> {
        self.data.memos.remove(&id);
        self.generation += 1;
        Ok(())
    }

    fn access(&self, memo_id: Uuid) -> Result<Option<AccessRecord>> {
        Ok(self.data.access.get(&memo_id).cloned())
    }

    fn put_access(&mut self, access: &AccessRecord) -> Result<()> {
        self.data.access.insert(access.memo_id, access.clone());
        self.generation += 1;
        Ok(())
    }

    fn delete_access(&mut self, memo_id: Uuid) -> Result<()> {
        self.data.access.remove(&memo_id);
        self.generation += 1;
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
        self.generation += 1;
        Ok(())
    }

    fn delete_state(&mut self, memo_id: Uuid) -> Result<()> {
        self.data.states.remove(&memo_id);
        self.generation += 1;
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
        Ok(self.data.blobs.get(&(id, kind)).map(|blob| {
            blob.used.set(self.tick());
            blob.envelope.to_vec()
        }))
    }

    fn put_blob(&mut self, id: Uuid, kind: AttachmentKind, envelope: &[u8]) -> Result<()> {
        let blob = Blob { envelope: envelope.into(), used: Cell::new(self.tick()) };
        self.data.blobs.insert((id, kind), blob);
        Ok(())
    }
}
