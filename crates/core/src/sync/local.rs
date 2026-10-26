//! Local edits, as the UI makes them. Each call is one store transaction; sync picks the
//! result up from the dirty state.

use uuid::Uuid;

use super::{
    ops::{self, transaction},
    store::{AttachmentRecord, MemoRecord, PushError, StateRecord, Store, UndecodedReason},
};
use crate::{
    Error, Result,
    account::UnlockedAccount,
    api::Permission,
    content::seal_attachment,
    envelope::AttachmentKind,
    keys::{AttachmentKey, MemoKey},
    payload::{AttachmentRef, MemoPayload, ThumbRef},
};

/// Creates a memo owned by the account. `now` (Unix ms) becomes `created_at`.
pub fn create_memo<S: Store>(store: &mut S, account: &UnlockedAccount, payload: MemoPayload, now: i64) -> Result<Uuid> {
    check_payload(&payload)?;
    let id = Uuid::now_v7();
    transaction(store, |store| ops::insert_new_memo(store, account, id, MemoKey::generate(), payload, now))?;
    Ok(id)
}

/// Replaces the memo's working copy. Setting it back to the synced content makes the memo clean.
pub fn edit_memo<S: Store>(store: &mut S, id: Uuid, payload: MemoPayload) -> Result<()> {
    check_payload(&payload)?;
    transaction(store, |store| {
        let mut memo = editable(store, id)?;
        if memo.conflict.is_some() {
            return Err(Error::InConflict);
        }
        memo.local = (memo.base.as_ref() != Some(&payload)).then_some(payload);
        if memo.push_error == Some(PushError::Invalid) {
            memo.push_error = None;
        }
        store.put_memo(&memo)
    })
}

/// Owner only; members leave with `DELETE /memos/{id}/members/me` instead. A memo that never
/// reached the server is removed right away.
pub fn delete_memo<S: Store>(store: &mut S, account: &UnlockedAccount, id: Uuid) -> Result<()> {
    transaction(store, |store| {
        let mut memo = existing(store, id)?;
        if memo.owner_id != account.user_id {
            return Err(Error::NotOwner);
        }
        if memo.server_version == 0 {
            return ops::remove_memo_locally(store, id);
        }
        // Deleting from the conflict view: the user has seen their version, so delete that one.
        if let Some(conflict) = memo.conflict.take() {
            memo.base = Some(conflict.theirs);
            memo.server_version = conflict.theirs_version;
            memo.key_version = conflict.theirs_key_version;
        }
        memo.pending_delete = true;
        store.put_memo(&memo)
    })
}

/// The overlap is settled: `resolved` is pushed on top of their version.
pub fn resolve_conflict<S: Store>(store: &mut S, id: Uuid, resolved: MemoPayload) -> Result<()> {
    check_payload(&resolved)?;
    transaction(store, |store| {
        let mut memo = existing(store, id)?;
        let conflict = memo.conflict.take().ok_or(Error::Malformed("no conflict to resolve"))?;
        memo.local = (resolved != conflict.theirs).then_some(resolved);
        memo.base = Some(conflict.theirs);
        memo.server_version = conflict.theirs_version;
        memo.key_version = conflict.theirs_key_version;
        store.put_memo(&memo)
    })
}

/// Saves the memo's current content as a new private memo, e.g. after losing write permission.
pub fn save_as_copy<S: Store>(store: &mut S, account: &UnlockedAccount, id: Uuid, now: i64) -> Result<Uuid> {
    transaction(store, |store| {
        let memo = existing(store, id)?;
        ops::restore_as_copy(store, account, &memo, now)?.ok_or(Error::ReadOnly)
    })
}

pub fn set_memo_state<S: Store>(store: &mut S, memo_id: Uuid, pinned: bool, archived: bool) -> Result<()> {
    transaction(store, |store| {
        existing(store, memo_id)?;
        store.put_state(&StateRecord { memo_id, pinned, archived, dirty: true })
    })
}

/// An image to attach. `thumbnail` is a small preview, already encoded (e.g. WebP).
#[derive(Clone, Debug)]
pub struct NewAttachment<'a> {
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: &'a [u8],
    pub thumbnail: Option<NewThumbnail<'a>>,
}

#[derive(Clone, Debug)]
pub struct NewThumbnail<'a> {
    pub mime: String,
    pub width: u32,
    pub height: u32,
    pub bytes: &'a [u8],
}

/// Encrypts and caches the blobs, queues their upload, and adds the reference to the memo's
/// working copy. The caller then inserts `![](attachment:<id>)` into the body with [`edit_memo`],
/// keeping the returned reference in the payload's `attachments`.
pub fn add_attachment<S: Store>(store: &mut S, memo_id: Uuid, new: NewAttachment<'_>) -> Result<AttachmentRef> {
    transaction(store, |store| {
        let mut memo = editable(store, memo_id)?;
        if memo.conflict.is_some() {
            return Err(Error::InConflict);
        }
        let (id, key) = (Uuid::now_v7(), AttachmentKey::generate());
        store.put_blob(
            id,
            AttachmentKind::Original,
            &seal_attachment(&key, id, AttachmentKind::Original, new.bytes),
        )?;
        if let Some(thumb) = &new.thumbnail {
            store.put_blob(
                id,
                AttachmentKind::Thumbnail,
                &seal_attachment(&key, id, AttachmentKind::Thumbnail, thumb.bytes),
            )?;
        }
        store.put_attachment(&AttachmentRecord {
            id,
            memo_id,
            on_server: false,
            uploaded: false,
            thumb_uploaded: false,
            upload_original: true,
            upload_thumbnail: new.thumbnail.is_some(),
            pending_delete: false,
        })?;
        let attachment = AttachmentRef {
            id,
            key,
            mime: new.mime,
            size: new.bytes.len() as u64,
            width: new.width,
            height: new.height,
            thumb: new.thumbnail.map(|t| ThumbRef { mime: t.mime, width: t.width, height: t.height }),
        };
        let mut payload = memo.current().cloned().unwrap_or_else(|| MemoPayload::new("", 0));
        payload.attachments.push(attachment.clone());
        memo.local = Some(payload);
        store.put_memo(&memo)?;
        Ok(attachment)
    })
}

/// Decrypted attachment bytes, if the blob is cached. Otherwise download it
/// (`GET /attachments/{id}/{kind}`), [`Store::put_blob`] the envelope, and call this again.
pub fn cached_attachment<S: Store>(
    store: &S,
    attachment: &AttachmentRef,
    kind: AttachmentKind,
) -> Result<Option<Vec<u8>>> {
    ops::cached_plaintext(store, attachment, kind)
}

/// Marks a contact as verified after the user compared fingerprints out of band.
pub fn verify_contact<S: Store>(store: &mut S, user_id: Uuid) -> Result<()> {
    transaction(store, |store| {
        let mut vault = store.vault()?.ok_or(Error::NotFound)?;
        if vault.newer_format {
            return Err(Error::ReadOnly);
        }
        let mut local = vault.current().clone();
        local.contacts.get_mut(&user_id).ok_or(Error::NotPinned(user_id))?.verified = true;
        vault.local = Some(local);
        store.put_vault(&vault)
    })
}

fn check_payload(payload: &MemoPayload) -> Result<()> {
    if !payload.is_editable() {
        return Err(Error::UnsupportedPayloadVersion(payload.v));
    }
    Ok(())
}

fn existing<S: Store>(store: &S, id: Uuid) -> Result<MemoRecord> {
    store.memo(id)?.filter(|m| !m.pending_delete).ok_or(Error::NotFound)
}

fn editable<S: Store>(store: &S, id: Uuid) -> Result<MemoRecord> {
    let memo = existing(store, id)?;
    let newer = memo.undecoded.as_ref().is_some_and(|u| u.reason == UndecodedReason::NewerFormat);
    let writable = store.access(id)?.is_some_and(|a| a.permission == Permission::Write);
    if newer || !writable || memo.push_error == Some(PushError::Forbidden) {
        return Err(Error::ReadOnly);
    }
    Ok(memo)
}
