//! Local edits, as the UI makes them. Each call is one store transaction; sync picks the
//! result up from the dirty state.

use uuid::Uuid;

use super::{
    ops::{self, transaction},
    store::{AttachmentRecord, MemoRecord, PushError, StateRecord, Store, UndecodedReason},
    view::{FILTER_PREFIX, MemoFilter, open_revision},
};
use crate::{
    Error, Result,
    account::UnlockedAccount,
    api::{Permission, Revision},
    content::seal_attachment,
    envelope::AttachmentKind,
    keys::{AttachmentKey, MemoKey},
    payload::{AttachmentRef, MemoPayload, ThumbRef},
    text,
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

/// Replaces the body of the working copy, as an editor does. `prune` also drops attachments the
/// body no longer refers to; pass it when the editor closes. Does nothing if nothing changed.
pub fn edit_body<S: Store>(store: &mut S, id: Uuid, body: &str, now: i64, prune: bool) -> Result<()> {
    let memo = existing(store, id)?;
    let current = memo.current().ok_or(Error::ReadOnly)?;
    let mut payload = current.clone();
    if payload.body != body {
        payload.body = body.into();
        payload.edited_at = now;
    }
    if prune {
        payload.prune_attachments();
    }
    if &payload == current {
        return Ok(());
    }
    edit_memo(store, id, payload)
}

/// Flips the task box at byte `offset` of the body (see [`text::toggle_task`]). False if there is
/// no box there any more.
pub fn toggle_task<S: Store>(store: &mut S, id: Uuid, offset: usize, now: i64) -> Result<bool> {
    let memo = existing(store, id)?;
    let body = memo.current().map(|p| p.body.as_str()).unwrap_or_default();
    match text::toggle_task(body, offset) {
        Some(body) => edit_body(store, id, &body, now, false).map(|()| true),
        None => Ok(false),
    }
}

/// Makes an old revision (`GET /memos/{id}/revisions/{version}`) the working copy; the next sync
/// pushes it as a new version.
pub fn restore_revision<S: Store>(store: &mut S, id: Uuid, revision: &Revision, now: i64) -> Result<()> {
    let mut payload = open_revision(store, id, revision)?;
    payload.edited_at = now;
    edit_memo(store, id, payload)
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

/// Settles a conflict with the body the user wrote, keeping the images either side has that it
/// refers to.
pub fn resolve_with_body<S: Store>(store: &mut S, id: Uuid, body: &str, now: i64) -> Result<()> {
    let memo = existing(store, id)?;
    let theirs = memo.conflict.as_ref().map(|c| c.theirs.clone()).ok_or(Error::Malformed("no conflict to resolve"))?;
    let mut resolved = MemoPayload::new(body, now);
    for attachment in memo.current().into_iter().flat_map(|p| &p.attachments).chain(&theirs.attachments) {
        if !resolved.attachments.iter().any(|a| a.id == attachment.id) {
            resolved.attachments.push(attachment.clone());
        }
    }
    resolved.prune_attachments();
    if resolved.body == theirs.body && resolved.attachments == theirs.attachments {
        resolved = theirs;
    }
    resolve_conflict(store, id, resolved)
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

/// [`cached_attachment`] for an attachment of a memo, found in any version the UI can show: the
/// working copy, the synced one, the other side of a conflict, or a read-only newer one.
pub fn memo_attachment<S: Store>(
    store: &S,
    memo_id: Uuid,
    attachment_id: Uuid,
    kind: AttachmentKind,
) -> Result<Option<Vec<u8>>> {
    let memo = store.memo(memo_id)?.ok_or(Error::NotFound)?;
    let payloads = [
        memo.local.as_ref(),
        memo.base.as_ref(),
        memo.conflict.as_ref().map(|c| &c.theirs),
        memo.undecoded.as_ref().and_then(|u| u.view.as_ref()),
    ];
    let attachment = payloads
        .into_iter()
        .flatten()
        .flat_map(|p| &p.attachments)
        .find(|a| a.id == attachment_id)
        .ok_or(Error::NotFound)?;
    cached_attachment(store, attachment, kind)
}

/// Anything local the server does not have yet: edits, states, the vault or image uploads.
pub fn has_unsynced<S: Store>(store: &S) -> Result<bool> {
    Ok(!store.dirty_memo_ids()?.is_empty()
        || !store.dirty_state_ids()?.is_empty()
        || store.vault()?.is_some_and(|v| v.local.is_some())
        || store.pending_attachments()?.iter().any(|a| a.upload_original || a.upload_thumbnail))
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

/// Saves the filter's query, tag and quick filters in the vault under `name`. Returns its id.
pub fn save_filter<S: Store>(store: &mut S, name: &str, filter: &MemoFilter) -> Result<Uuid> {
    let id = Uuid::now_v7();
    let value = serde_json::json!({ "name": name.trim(), "query": filter.query, "tag": filter.tag, "has": filter.has });
    edit_settings(store, |settings| {
        settings.insert(format!("{FILTER_PREFIX}{id}"), value);
    })?;
    Ok(id)
}

pub fn delete_filter<S: Store>(store: &mut S, id: Uuid) -> Result<()> {
    edit_settings(store, |settings| {
        settings.remove(&format!("{FILTER_PREFIX}{id}"));
    })
}

fn edit_settings<S: Store>(
    store: &mut S,
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>),
) -> Result<()> {
    transaction(store, |store| {
        let mut vault = store.vault()?.ok_or(Error::NotFound)?;
        if vault.newer_format {
            return Err(Error::ReadOnly);
        }
        let mut local = vault.current().clone();
        edit(&mut local.settings);
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
