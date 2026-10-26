//! The sync round (`docs/sync.md#client-sync-round`), without I/O: the caller sends
//! [`SyncSession::request`] to `POST /api/v1/sync` and hands the response to
//! [`SyncSession::handle`] until it returns [`Step::Done`]. If a request or `handle` fails, drop
//! the session; nothing is lost, the next session rebuilds the push from the dirty state.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::{
    ops::{self, PinCheck, transaction},
    store::{
        AccessRecord, AttachmentRecord, Conflict, InFlight, MemoRecord, PushError, StateRecord, Store, SyncMeta,
        Undecoded, UndecodedReason, VaultRecord,
    },
};
use crate::{
    Error, Result,
    account::UnlockedAccount,
    api::{
        AccessChange, AttachmentChange, Change, MAX_PUSH_OPS, MemoChange, OpResult, OpStatus, Permission, PushOp,
        StateChange, SyncRequest, SyncResponse, VaultChange,
    },
    content::{MemoContext, open_memo, open_vault, seal_memo, seal_vault},
    envelope::AttachmentKind,
    keys::PublicKey,
    merge::{MemoMerge, PinConflict, merge_memo, merge_vault},
    payload::{MemoPayload, VaultPayload},
    share::{KeyGrant, unwrap_memo_key, wrap_memo_key},
};

/// Push rounds per session: merges and pushes over [`MAX_PUSH_OPS`] need more than one.
pub const MAX_ROUNDS: u32 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send [`SyncSession::request`] again.
    Send,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncEvent {
    /// The server asked for a full resync (backup restored, or offline for longer than the
    /// tombstone retention).
    Reset,
    /// Overlapping edits. The memo waits for [`resolve_conflict`](super::resolve_conflict).
    Conflict {
        memo_id: Uuid,
    },
    /// A local edit could not go to the original memo, so it was saved as a new private memo.
    RestoredCopy {
        original: Uuid,
        copy: Uuid,
        reason: CopyReason,
    },
    /// A local delete was cancelled because the memo changed in the meantime.
    DeleteRejected {
        memo_id: Uuid,
    },
    PushRejected {
        memo_id: Uuid,
        reason: PushError,
    },
    /// The server presented a version older than one already seen. Ignored.
    Rollback {
        memo_id: Uuid,
        version: u64,
        highest_seen: u64,
    },
    /// During a reset: the server had older data than this device. The local content goes back up.
    ServerLostData {
        memo_id: Uuid,
    },
    CannotDecrypt {
        memo_id: Uuid,
    },
    NewerFormat {
        memo_id: Uuid,
    },
    /// A wrapped key did not verify against the owner's pinned key. Possible attack.
    KeyRejected {
        memo_id: Uuid,
        owner_id: Uuid,
    },
    /// The server presented a different public key for a pinned user. Possible attack.
    KeyChanged {
        user_id: Uuid,
    },
    /// Two devices pinned different keys for the same user. Possible attack.
    PinConflict(PinConflict),
    VaultNewerFormat,
    VaultUndecryptable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyReason {
    Deleted,
    AccessRevoked,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub events: Vec<SyncEvent>,
}

impl SyncReport {
    fn push(&mut self, event: SyncEvent) {
        if !self.events.contains(&event) {
            self.events.push(event);
        }
    }
}

/// What each op of the current push carried, to apply its result.
#[derive(Debug)]
enum Pending {
    MemoPut { id: Uuid, version: u64, key_version: u32, snapshot: MemoPayload },
    MemoDelete { id: Uuid },
    State { memo_id: Uuid, pinned: bool, archived: bool },
    AttachmentDelete { id: Uuid },
    Vault { version: u64, snapshot: VaultPayload },
}

#[derive(Debug, Default)]
struct ResetSeen {
    memos: BTreeSet<Uuid>,
    attachments: BTreeSet<Uuid>,
}

#[derive(Debug)]
pub struct SyncSession {
    now: i64,
    request: SyncRequest,
    pending: Vec<Pending>,
    round: u32,
    resets: u32,
    /// `Some` while a reset is running.
    seen: Option<ResetSeen>,
    /// Something needs another round (a merge made memos dirty, the push was truncated).
    again: bool,
    report: SyncReport,
}

impl SyncSession {
    /// Builds the first request. `now` (Unix ms) timestamps contact pins and restored copies.
    pub fn start<S: Store>(store: &mut S, account: &UnlockedAccount, now: i64) -> Result<Self> {
        let mut session = Self {
            now,
            request: SyncRequest { cursor: 0, epoch: None, push: Vec::new() },
            pending: Vec::new(),
            round: 0,
            resets: 0,
            seen: None,
            again: false,
            report: SyncReport::default(),
        };
        transaction(store, |store| {
            let mut meta = store.meta()?;
            if meta.resetting {
                meta.cursor = 0;
                store.set_meta(&meta)?;
                session.seen = Some(ResetSeen::default());
            }
            for id in store.undecoded_memo_ids()? {
                if let Some(memo) = store.memo(id)? {
                    session.decode(store, memo)?;
                }
            }
            session.next_round(store, account)
        })?;
        Ok(session)
    }

    pub fn request(&self) -> &SyncRequest {
        &self.request
    }

    pub fn report(&self) -> &SyncReport {
        &self.report
    }

    pub fn into_report(self) -> SyncReport {
        self.report
    }

    pub fn handle<S: Store>(
        &mut self,
        store: &mut S,
        account: &UnlockedAccount,
        response: SyncResponse,
    ) -> Result<Step> {
        if response.reset {
            return self.begin_reset(store, response.epoch);
        }
        if response.results.len() != self.pending.len() {
            return Err(Error::Malformed("sync results"));
        }
        let pending = std::mem::take(&mut self.pending);
        transaction(store, |store| {
            for (op, result) in pending.into_iter().zip(response.results) {
                self.apply_result(store, account, op, result)?;
            }
            // The vault first, so contact pins are known before access entities are verified.
            let (vaults, others): (Vec<_>, Vec<_>) =
                response.changes.into_iter().partition(|c| matches!(c, Change::Vault(_)));
            for change in vaults.into_iter().chain(others) {
                self.apply_change(store, account, change)?;
            }
            let mut meta = store.meta()?;
            meta.cursor = response.cursor;
            meta.epoch = Some(response.epoch.clone());
            if !response.more
                && let Some(seen) = self.seen.take()
            {
                self.finish_reset(store, account, &seen, meta.server_restored)?;
                meta.resetting = false;
                meta.server_restored = false;
            }
            store.set_meta(&meta)?;
            if response.more {
                self.request = SyncRequest { cursor: meta.cursor, epoch: meta.epoch, push: Vec::new() };
                return Ok(Step::Send);
            }
            if self.again && self.round < MAX_ROUNDS {
                self.next_round(store, account)?;
                if !self.request.push.is_empty() {
                    return Ok(Step::Send);
                }
            }
            Ok(Step::Done)
        })
    }

    fn begin_reset<S: Store>(&mut self, store: &mut S, epoch: String) -> Result<Step> {
        self.resets += 1;
        if self.resets > 1 {
            return Err(Error::Malformed("repeated sync reset"));
        }
        let meta = transaction(store, |store| {
            let old = store.meta()?;
            let server_restored = old.server_restored || old.epoch.as_ref().is_some_and(|e| *e != epoch);
            let meta = SyncMeta { cursor: 0, epoch: Some(epoch), resetting: true, server_restored };
            store.set_meta(&meta)?;
            Ok(meta)
        })?;
        self.report.push(SyncEvent::Reset);
        self.seen = Some(ResetSeen::default());
        self.pending.clear();
        self.request = SyncRequest { cursor: 0, epoch: meta.epoch, push: Vec::new() };
        Ok(Step::Send)
    }

    fn next_round<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount) -> Result<()> {
        self.round += 1;
        self.again = false;
        let meta = store.meta()?;
        let (push, pending) =
            if self.seen.is_some() { (Vec::new(), Vec::new()) } else { self.build_push(store, account)? };
        self.request = SyncRequest { cursor: meta.cursor, epoch: meta.epoch, push };
        self.pending = pending;
        Ok(())
    }

    // ---- Push ----

    fn build_push<S: Store>(
        &mut self,
        store: &mut S,
        account: &UnlockedAccount,
    ) -> Result<(Vec<PushOp>, Vec<Pending>)> {
        let mut ops = Vec::new();
        let mut pending = Vec::new();

        if let Some(vault) = store.vault()?
            && let Some(local) = vault.local.as_ref().filter(|_| !vault.newer_format)
        {
            let version = vault.server_version + 1;
            let ciphertext = seal_vault(&account.account_key, account.user_id, version, local)?;
            ops.push(PushOp::VaultPut { base_version: vault.server_version, ciphertext });
            pending.push(Pending::Vault { version, snapshot: local.clone() });
        }

        let mut created = BTreeSet::new();
        for id in store.dirty_memo_ids()? {
            if ops.len() >= MAX_PUSH_OPS {
                self.again = true;
                break;
            }
            let Some(mut memo) = store.memo(id)? else { continue };
            let blocked = match memo.push_error {
                Some(PushError::Forbidden | PushError::Invalid) => true,
                Some(PushError::QuotaExceeded) => self.round > 1,
                None => false,
            };
            if blocked || memo.conflict.is_some() || memo.undecoded.is_some() {
                continue;
            }
            if memo.pending_delete {
                if memo.server_version == 0 {
                    ops::remove_memo_locally(store, id)?;
                } else {
                    ops.push(PushOp::MemoDelete { id, base_version: memo.server_version });
                    pending.push(Pending::MemoDelete { id });
                }
                continue;
            }
            let Some(local) = memo.local.clone() else { continue };
            let Some(access) = store.access(id)?.filter(|a| a.permission == Permission::Write) else { continue };
            let Some(key) = access.keys.get(&memo.key_version) else { continue };
            let version = memo.server_version + 1;
            let ctx = MemoContext { memo_id: id, version, key_version: memo.key_version };
            let ciphertext = seal_memo(key, &ctx, &local)?;
            let wrapped_key = (memo.server_version == 0).then(|| {
                let grant =
                    KeyGrant { memo_id: id, key_version: 1, owner_id: account.user_id, member_id: account.user_id };
                wrap_memo_key(&account.keypair, account.keypair.public_key(), &grant, key)
            });
            if wrapped_key.is_some() {
                created.insert(id);
            }
            memo.in_flight = Some(InFlight::record(memo.in_flight.take(), version, local.clone()));
            store.put_memo(&memo)?;
            ops.push(PushOp::MemoPut {
                id,
                base_version: memo.server_version,
                key_version: memo.key_version,
                ciphertext,
                wrapped_key,
            });
            pending.push(Pending::MemoPut { id, version, key_version: memo.key_version, snapshot: local });
        }

        for memo_id in store.dirty_state_ids()? {
            if ops.len() >= MAX_PUSH_OPS {
                self.again = true;
                break;
            }
            let on_server = created.contains(&memo_id) || store.memo(memo_id)?.is_some_and(|m| m.server_version > 0);
            let Some(state) = store.state(memo_id)?.filter(|_| on_server) else { continue };
            ops.push(PushOp::StatePut { memo_id, pinned: state.pinned, archived: state.archived });
            pending.push(Pending::State { memo_id, pinned: state.pinned, archived: state.archived });
        }

        for attachment in store.pending_attachments()? {
            if !attachment.pending_delete {
                continue;
            }
            if ops.len() >= MAX_PUSH_OPS {
                self.again = true;
                break;
            }
            ops.push(PushOp::AttachmentDelete { id: attachment.id });
            pending.push(Pending::AttachmentDelete { id: attachment.id });
        }
        Ok((ops, pending))
    }

    fn apply_result<S: Store>(
        &mut self,
        store: &mut S,
        account: &UnlockedAccount,
        op: Pending,
        result: OpResult,
    ) -> Result<()> {
        match op {
            Pending::MemoPut { id, version, key_version, snapshot } => {
                let Some(mut memo) = store.memo(id)? else { return Ok(()) };
                let reason = match result.status {
                    OpStatus::Ok => {
                        let old_base = memo.base.replace(snapshot.clone());
                        memo.server_version = version;
                        memo.key_version = key_version;
                        memo.highest_seen_version = memo.highest_seen_version.max(version);
                        if memo.local.as_ref() == Some(&snapshot) {
                            memo.local = None;
                        }
                        memo.in_flight = None;
                        memo.push_error = None;
                        memo.updated_at = self.now;
                        memo.updated_by = Some(account.user_id);
                        store.put_memo(&memo)?;
                        self.again |= release_attachments(store, &memo, old_base.as_ref())?;
                        return Ok(());
                    }
                    // The newer version is in `changes`.
                    OpStatus::Conflict => return Ok(()),
                    OpStatus::Deleted => return self.lose_memo(store, account, memo, CopyReason::Deleted),
                    OpStatus::NotFound => return self.lose_memo(store, account, memo, CopyReason::AccessRevoked),
                    OpStatus::Forbidden => PushError::Forbidden,
                    OpStatus::Invalid => PushError::Invalid,
                    OpStatus::QuotaExceeded => PushError::QuotaExceeded,
                };
                memo.in_flight = None;
                memo.push_error = Some(reason);
                store.put_memo(&memo)?;
                self.report.push(SyncEvent::PushRejected { memo_id: id, reason });
            }
            Pending::MemoDelete { id } => match result.status {
                OpStatus::Ok | OpStatus::Deleted | OpStatus::NotFound => ops::remove_memo_locally(store, id)?,
                OpStatus::Conflict | OpStatus::Forbidden | OpStatus::Invalid | OpStatus::QuotaExceeded => {
                    if let Some(mut memo) = store.memo(id)? {
                        memo.pending_delete = false;
                        store.put_memo(&memo)?;
                    }
                    self.report.push(SyncEvent::DeleteRejected { memo_id: id });
                }
            },
            // Last write wins and failures are not retried: only a newer local change stays dirty.
            Pending::State { memo_id, pinned, archived } => {
                if let Some(mut state) = store.state(memo_id)?.filter(|s| s.pinned == pinned && s.archived == archived)
                {
                    state.dirty = false;
                    store.put_state(&state)?;
                }
            }
            Pending::AttachmentDelete { id } => store.delete_attachment(id)?,
            Pending::Vault { version, snapshot } => {
                if result.status == OpStatus::Ok
                    && let Some(mut vault) = store.vault()?
                {
                    vault.server_version = version;
                    if vault.local.as_ref() == Some(&snapshot) {
                        vault.local = None;
                    }
                    vault.base = snapshot;
                    self.again |= vault.local.is_some();
                    store.put_vault(&vault)?;
                }
            }
        }
        Ok(())
    }

    // ---- Changes ----

    fn apply_change<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount, change: Change) -> Result<()> {
        match change {
            Change::Memo(change) => self.apply_memo(store, account, change),
            Change::Access(change) => self.apply_access(store, account, change),
            Change::Attachment(change) => self.apply_attachment(store, change),
            Change::State(change) => apply_state(store, change),
            Change::Vault(change) => self.apply_vault(store, account, change),
        }
    }

    fn apply_memo<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount, change: MemoChange) -> Result<()> {
        if let Some(seen) = &mut self.seen {
            seen.memos.insert(change.id);
        }
        let existing = store.memo(change.id)?;
        if change.deleted {
            if let Some(memo) = existing {
                self.lose_memo(store, account, memo, CopyReason::Deleted)?;
            }
            return Ok(());
        }
        let ciphertext = change.ciphertext.ok_or(Error::Malformed("memo change without ciphertext"))?;
        let resetting = self.seen.is_some();
        let mut memo = existing.unwrap_or_else(|| MemoRecord {
            id: change.id,
            // The owner never changes; a record keeps the first one it saw.
            owner_id: change.owner_id,
            server_version: 0,
            key_version: change.key_version,
            base: None,
            local: None,
            highest_seen_version: 0,
            pending_delete: false,
            conflict: None,
            undecoded: None,
            push_error: None,
            in_flight: None,
            created_at: change.created_at,
            updated_at: change.updated_at,
            updated_by: change.updated_by,
        });
        if !resetting {
            if change.version < memo.highest_seen_version {
                let highest_seen = memo.highest_seen_version;
                self.report.push(SyncEvent::Rollback { memo_id: memo.id, version: change.version, highest_seen });
                return Ok(());
            }
            let known = memo.server_version.max(memo.undecoded.as_ref().map_or(0, |u| u.version));
            if change.version <= known {
                return Ok(());
            }
        }
        memo.highest_seen_version =
            if resetting { change.version } else { memo.highest_seen_version.max(change.version) };
        memo.updated_at = change.updated_at;
        memo.updated_by = change.updated_by;
        memo.undecoded = Some(Undecoded {
            version: change.version,
            key_version: change.key_version,
            ciphertext,
            reason: UndecodedReason::MissingKey,
            view: None,
        });
        self.decode(store, memo)
    }

    /// Decrypts `memo.undecoded` if possible and integrates it; otherwise records why not.
    fn decode<S: Store>(&mut self, store: &mut S, mut memo: MemoRecord) -> Result<()> {
        let Some(undecoded) = memo.undecoded.take() else { return Ok(()) };
        let previous = undecoded.reason;
        let key = store.access(memo.id)?.and_then(|a| a.keys.get(&undecoded.key_version).cloned());
        let (reason, view) = match key {
            None => (UndecodedReason::MissingKey, None),
            Some(key) => {
                let ctx =
                    MemoContext { memo_id: memo.id, version: undecoded.version, key_version: undecoded.key_version };
                match open_memo(&key, &ctx, &undecoded.ciphertext) {
                    Ok(payload) if payload.is_editable() => {
                        return self.integrate(store, memo, undecoded.version, undecoded.key_version, payload);
                    }
                    Ok(payload) => (UndecodedReason::NewerFormat, Some(payload)),
                    Err(Error::UnsupportedPayloadVersion(_)) => (UndecodedReason::NewerFormat, None),
                    Err(_) => (UndecodedReason::Undecryptable, None),
                }
            }
        };
        if reason != previous {
            match reason {
                UndecodedReason::NewerFormat => self.report.push(SyncEvent::NewerFormat { memo_id: memo.id }),
                UndecodedReason::Undecryptable => self.report.push(SyncEvent::CannotDecrypt { memo_id: memo.id }),
                UndecodedReason::MissingKey => {}
            }
        }
        memo.undecoded = Some(Undecoded { reason, view, ..undecoded });
        store.put_memo(&memo)
    }

    fn integrate<S: Store>(
        &mut self,
        store: &mut S,
        mut memo: MemoRecord,
        version: u64,
        key_version: u32,
        theirs: MemoPayload,
    ) -> Result<()> {
        let resetting = self.seen.is_some();
        let in_flight = memo.in_flight.take_if(|f| f.version <= version);
        if in_flight.is_some_and(|f| f.version == version && f.payloads.contains(&theirs)) {
            // Our own write, whose result we did not get. `local` is that write or a later edit of it.
            if memo.local.as_ref() == Some(&theirs) {
                memo.local = None;
            }
            self.again |= memo.local.is_some();
            set_base(&mut memo, version, key_version, theirs);
            return store.put_memo(&memo);
        }

        if resetting && memo.base.is_some() && version <= memo.server_version {
            let ours = memo.current().cloned().expect("base is set");
            if version < memo.server_version {
                // The server lost data: push our content back on top of what it has.
                self.report.push(SyncEvent::ServerLostData { memo_id: memo.id });
                memo.local = (ours != theirs).then_some(ours);
                self.again |= memo.local.is_some();
            } else if memo.base.as_ref() != Some(&theirs) {
                // The same version with other content (rewritten after a restore). Let the user pick.
                memo.local = Some(ours);
                memo.conflict = Some(Conflict { theirs, theirs_version: version, theirs_key_version: key_version });
                self.report.push(SyncEvent::Conflict { memo_id: memo.id });
                return store.put_memo(&memo);
            }
            set_base(&mut memo, version, key_version, theirs);
            return store.put_memo(&memo);
        }

        if memo.pending_delete {
            memo.pending_delete = false;
            self.report.push(SyncEvent::DeleteRejected { memo_id: memo.id });
        }
        if let Some(conflict) = &mut memo.conflict {
            *conflict = Conflict { theirs, theirs_version: version, theirs_key_version: key_version };
            return store.put_memo(&memo);
        }
        if let Some(ours) = memo.local.take() {
            let base = memo.base.clone().unwrap_or_else(|| MemoPayload::new("", 0));
            match merge_memo(&base, &ours, &theirs) {
                MemoMerge::Clean(merged) => {
                    memo.local = (merged != theirs).then_some(merged);
                    self.again |= memo.local.is_some();
                }
                MemoMerge::Conflict => {
                    memo.local = Some(ours);
                    memo.conflict = Some(Conflict { theirs, theirs_version: version, theirs_key_version: key_version });
                    self.report.push(SyncEvent::Conflict { memo_id: memo.id });
                    return store.put_memo(&memo);
                }
            }
        }
        set_base(&mut memo, version, key_version, theirs);
        store.put_memo(&memo)
    }

    fn apply_access<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount, change: AccessChange) -> Result<()> {
        let memo_id = change.memo_id;
        if let Some(seen) = &mut self.seen {
            seen.memos.insert(memo_id);
        }
        if change.revoked {
            match store.memo(memo_id)? {
                Some(memo) => self.lose_memo(store, account, memo, CopyReason::AccessRevoked)?,
                None => ops::remove_memo_locally(store, memo_id)?,
            }
            return Ok(());
        }
        let owner_id = change.owner_id.ok_or(Error::Malformed("access change without owner"))?;
        let permission = change.permission.ok_or(Error::Malformed("access change without permission"))?;
        let mut access = store.access(memo_id)?.unwrap_or_else(|| AccessRecord {
            memo_id,
            owner_id,
            permission,
            members: Vec::new(),
            keys: Default::default(),
        });

        // Trust on first use for everyone listed; the owner's pinned key verifies the wrapped keys.
        let own_key = *account.keypair.public_key();
        let mut owner_key = (access.owner_id == account.user_id).then_some(own_key);
        for member in &change.members {
            let key = if member.user_id == account.user_id {
                if member.public_key != own_key {
                    self.report.push(SyncEvent::KeyChanged { user_id: member.user_id });
                }
                own_key
            } else {
                self.check_pin(store, member.user_id, &member.username, member.public_key)?
            };
            if member.user_id == access.owner_id {
                owner_key = Some(key);
            }
        }
        let owner_key = match owner_key {
            Some(key) => Some(key),
            None => ops::pinned_key(store, access.owner_id)?,
        };
        for wrapped in change.keys {
            if access.keys.contains_key(&wrapped.key_version) {
                continue;
            }
            let grant = KeyGrant {
                memo_id,
                key_version: wrapped.key_version,
                owner_id: access.owner_id,
                member_id: account.user_id,
            };
            match owner_key.map(|owner_key| unwrap_memo_key(&account.keypair, &owner_key, &grant, &wrapped.wrapped_key))
            {
                Some(Ok(key)) => {
                    access.keys.insert(wrapped.key_version, key);
                }
                _ => self.report.push(SyncEvent::KeyRejected { memo_id, owner_id: access.owner_id }),
            }
        }
        access.permission = permission;
        access.members = change.members;
        store.put_access(&access)?;

        if let Some(mut memo) = store.memo(memo_id)? {
            if permission == Permission::Write && memo.push_error == Some(PushError::Forbidden) {
                memo.push_error = None;
                self.again |= memo.is_dirty();
                store.put_memo(&memo)?;
            }
            if memo.undecoded.is_some() {
                self.decode(store, memo)?;
            }
        }
        Ok(())
    }

    /// Returns the key to trust for `user_id`: the pinned one.
    fn check_pin<S: Store>(
        &mut self,
        store: &mut S,
        user_id: Uuid,
        username: &str,
        presented: PublicKey,
    ) -> Result<PublicKey> {
        match ops::check_pin(store, user_id, username, presented, self.now)? {
            PinCheck::Matches | PinCheck::Unavailable => Ok(presented),
            PinCheck::Pinned => {
                self.again = true;
                Ok(presented)
            }
            PinCheck::Changed(pinned) => {
                self.report.push(SyncEvent::KeyChanged { user_id });
                Ok(pinned)
            }
        }
    }

    fn apply_attachment<S: Store>(&mut self, store: &mut S, change: AttachmentChange) -> Result<()> {
        if let Some(seen) = &mut self.seen {
            seen.attachments.insert(change.id);
        }
        if change.deleted {
            return store.delete_attachment(change.id);
        }
        let mut attachment = store.attachment(change.id)?.unwrap_or(AttachmentRecord {
            id: change.id,
            memo_id: change.memo_id,
            on_server: true,
            uploaded: false,
            thumb_uploaded: false,
            upload_original: false,
            upload_thumbnail: false,
            pending_delete: false,
        });
        attachment.on_server = true;
        attachment.uploaded = change.uploaded;
        attachment.thumb_uploaded = change.thumb_uploaded;
        // The server already has the bytes (an upload whose response was lost, for example).
        attachment.upload_original &= !change.uploaded;
        attachment.upload_thumbnail &= !change.thumb_uploaded;
        store.put_attachment(&attachment)
    }

    fn apply_vault<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount, change: VaultChange) -> Result<()> {
        let mut vault = store.vault()?.unwrap_or(VaultRecord {
            server_version: 0,
            base: VaultPayload::default(),
            local: None,
            newer_format: false,
        });
        let resetting = self.seen.is_some();
        if !resetting && change.version <= vault.server_version {
            return Ok(());
        }
        let theirs = match open_vault(&account.account_key, account.user_id, change.version, &change.ciphertext) {
            Ok(theirs) => theirs,
            Err(Error::UnsupportedPayloadVersion(_)) => {
                vault.newer_format = true;
                vault.server_version = change.version;
                self.report.push(SyncEvent::VaultNewerFormat);
                return store.put_vault(&vault);
            }
            Err(_) => {
                self.report.push(SyncEvent::VaultUndecryptable);
                return Ok(());
            }
        };
        // After a server data loss, rebase everything we have onto what the server returned.
        let (base, ours) = if resetting && change.version < vault.server_version {
            (theirs.clone(), Some(vault.current().clone()))
        } else {
            (vault.base.clone(), vault.local.take())
        };
        vault.local = match ours {
            Some(ours) => {
                let (merged, conflicts) = merge_vault(&base, &ours, &theirs);
                for conflict in conflicts {
                    self.report.push(SyncEvent::PinConflict(conflict));
                }
                (merged != theirs).then_some(merged)
            }
            None => None,
        };
        self.again |= vault.local.is_some();
        vault.base = theirs;
        vault.server_version = change.version;
        vault.newer_format = false;
        store.put_vault(&vault)
    }

    /// Turns a memo the restored server does not know back into an unsynced one with the same id
    /// and `memo_key[1]`. Sharing is lost with the server's data; attachments are uploaded again.
    fn recreate<S: Store>(&mut self, store: &mut S, account: &UnlockedAccount, mut memo: MemoRecord) -> Result<bool> {
        let Some(mut access) = store.access(memo.id)? else { return Ok(false) };
        let (Some(content), Some(key)) =
            (memo.current().filter(|p| p.is_editable()).cloned(), access.keys.get(&1).cloned())
        else {
            return Ok(false);
        };
        memo = MemoRecord {
            server_version: 0,
            key_version: 1,
            base: None,
            local: Some(content),
            highest_seen_version: 0,
            pending_delete: false,
            conflict: None,
            undecoded: None,
            push_error: None,
            in_flight: None,
            ..memo
        };
        access.keys = [(1, key)].into();
        access.members.retain(|m| m.user_id == account.user_id);
        access.permission = Permission::Write;
        for mut attachment in store.attachments_of(memo.id)? {
            attachment.on_server = false;
            attachment.uploaded = false;
            attachment.thumb_uploaded = false;
            attachment.upload_original = store.blob(attachment.id, AttachmentKind::Original)?.is_some();
            attachment.upload_thumbnail = store.blob(attachment.id, AttachmentKind::Thumbnail)?.is_some();
            store.put_attachment(&attachment)?;
        }
        store.put_access(&access)?;
        store.put_memo(&memo)?;
        self.report.push(SyncEvent::ServerLostData { memo_id: memo.id });
        Ok(true)
    }

    /// The memo is gone from the server or no longer ours to see. Unsynced edits survive as a copy.
    fn lose_memo<S: Store>(
        &mut self,
        store: &mut S,
        account: &UnlockedAccount,
        memo: MemoRecord,
        reason: CopyReason,
    ) -> Result<()> {
        if memo.local.is_some()
            && !memo.pending_delete
            && let Some(copy) = ops::restore_as_copy(store, account, &memo, self.now)?
        {
            self.report.push(SyncEvent::RestoredCopy { original: memo.id, copy, reason });
            self.again = true;
        }
        ops::remove_memo_locally(store, memo.id)
    }

    /// Everything the full resync did not return is gone from the server. After a restore, our own
    /// memos are created again under the same id; otherwise they were deleted while this device was
    /// offline for longer than the tombstone retention.
    fn finish_reset<S: Store>(
        &mut self,
        store: &mut S,
        account: &UnlockedAccount,
        seen: &ResetSeen,
        restored: bool,
    ) -> Result<()> {
        for id in store.memo_ids()? {
            if seen.memos.contains(&id) {
                continue;
            }
            let Some(memo) = store.memo(id)?.filter(|m| m.server_version > 0) else { continue };
            if restored && memo.owner_id == account.user_id && self.recreate(store, account, memo.clone())? {
                continue;
            }
            self.lose_memo(store, account, memo, CopyReason::Deleted)?;
        }
        for id in store.attachment_ids()? {
            if seen.attachments.contains(&id) {
                continue;
            }
            let Some(attachment) = store.attachment(id)? else { continue };
            let not_uploaded_yet = attachment.upload_original || attachment.upload_thumbnail || !attachment.on_server;
            if !(not_uploaded_yet && store.memo(attachment.memo_id)?.is_some()) {
                store.delete_attachment(id)?;
            }
        }
        self.again = true;
        Ok(())
    }
}

fn set_base(memo: &mut MemoRecord, version: u64, key_version: u32, payload: MemoPayload) {
    memo.base = Some(payload);
    memo.server_version = version;
    memo.key_version = key_version;
}

fn apply_state<S: Store>(store: &mut S, change: StateChange) -> Result<()> {
    if store.state(change.memo_id)?.is_some_and(|s| s.dirty) {
        return Ok(());
    }
    store.put_state(&StateRecord {
        memo_id: change.memo_id,
        pinned: change.pinned,
        archived: change.archived,
        dirty: false,
    })
}

/// After our write is accepted: attachments it removed are deleted on the server, and attachments
/// added and removed again before ever syncing are dropped. Others are left alone; they may belong
/// to a member's edit this device has not seen yet. Returns whether a delete was queued.
fn release_attachments<S: Store>(store: &mut S, memo: &MemoRecord, old_base: Option<&MemoPayload>) -> Result<bool> {
    let mut queued = false;
    let refs = |p: Option<&MemoPayload>| {
        p.into_iter().flat_map(|p| p.attachments.iter().map(|a| a.id)).collect::<BTreeSet<_>>()
    };
    let referenced: BTreeSet<_> = refs(memo.base.as_ref()).union(&refs(memo.local.as_ref())).copied().collect();
    let previously = refs(old_base);
    for mut attachment in store.attachments_of(memo.id)? {
        if referenced.contains(&attachment.id) {
            continue;
        }
        let ours_unsynced = attachment.upload_original || attachment.upload_thumbnail;
        if !previously.contains(&attachment.id) && !ours_unsynced {
            continue;
        }
        if attachment.on_server {
            attachment.pending_delete = true;
            attachment.upload_original = false;
            attachment.upload_thumbnail = false;
            store.put_attachment(&attachment)?;
            queued = true;
        } else {
            store.delete_attachment(attachment.id)?;
        }
    }
    Ok(queued)
}

/// One blob to `PUT /memos/{memo_id}/attachments/{attachment_id}/{kind}`; the body is
/// [`Store::blob`]. Call [`upload_done`] after a `200` or `201`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upload {
    pub memo_id: Uuid,
    pub attachment_id: Uuid,
    pub kind: AttachmentKind,
}

/// Blobs to upload, for memos that exist on the server.
pub fn pending_uploads<S: Store>(store: &S) -> Result<Vec<Upload>> {
    let mut uploads = Vec::new();
    for attachment in store.pending_attachments()? {
        if !store.memo(attachment.memo_id)?.is_some_and(|m| m.server_version > 0) {
            continue;
        }
        for kind in [AttachmentKind::Original, AttachmentKind::Thumbnail] {
            if attachment.pending_upload(kind) {
                uploads.push(Upload { memo_id: attachment.memo_id, attachment_id: attachment.id, kind });
            }
        }
    }
    Ok(uploads)
}

pub fn upload_done<S: Store>(store: &mut S, attachment_id: Uuid, kind: AttachmentKind) -> Result<()> {
    transaction(store, |store| {
        let Some(mut attachment) = store.attachment(attachment_id)? else { return Ok(()) };
        attachment.on_server = true;
        match kind {
            AttachmentKind::Original => (attachment.upload_original, attachment.uploaded) = (false, true),
            AttachmentKind::Thumbnail => (attachment.upload_thumbnail, attachment.thumb_uploaded) = (false, true),
        }
        store.put_attachment(&attachment)
    })
}
