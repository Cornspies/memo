//! Store operations shared by the engine, the local edit API and sharing.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use super::store::{AccessRecord, AttachmentRecord, MemoRecord, Store, VaultRecord};
use crate::{
    Error, Result,
    account::UnlockedAccount,
    api::{Member, Permission},
    content::{open_attachment, seal_attachment},
    envelope::AttachmentKind,
    keys::{AttachmentKey, MemoKey, PublicKey},
    payload::{AttachmentRef, Contact, MemoPayload, VaultPayload},
};

/// Runs `f` in a store transaction, rolling back on error.
pub(crate) fn transaction<S: Store + ?Sized, R>(store: &mut S, f: impl FnOnce(&mut S) -> Result<R>) -> Result<R> {
    store.begin()?;
    match f(store).and_then(|r| store.commit().map(|()| r)) {
        Ok(r) => Ok(r),
        Err(e) => {
            store.rollback();
            Err(e)
        }
    }
}

/// Removes a memo and everything that hangs off it from this device. Returns whether that made
/// the vault dirty (the memo's share record goes too).
pub(crate) fn remove_memo_locally<S: Store + ?Sized>(store: &mut S, id: Uuid) -> Result<bool> {
    for attachment in store.attachments_of(id)? {
        store.delete_attachment(attachment.id)?;
    }
    store.delete_state(id)?;
    store.delete_access(id)?;
    store.delete_memo(id)?;
    forget_shares(store, id, None)
}

/// The memo's owner: the one its access record names, which the wrapped keys verify. Until that
/// record is there, what the server said.
pub(crate) fn owner_of<S: Store + ?Sized>(store: &S, memo: &MemoRecord) -> Result<Uuid> {
    Ok(store.access(memo.id)?.map_or(memo.owner_id, |a| a.owner_id))
}

/// Who the account shared the memo with, by its own record in the vault.
pub(crate) fn shared_with<S: Store + ?Sized>(store: &S, memo_id: Uuid) -> Result<BTreeSet<Uuid>> {
    Ok(store.vault()?.and_then(|v| v.current().shares.get(&memo_id).cloned()).unwrap_or_default())
}

/// Records in the vault that the account shares `memo_id` with `user_id`.
pub(crate) fn record_share<S: Store + ?Sized>(store: &mut S, memo_id: Uuid, user_id: Uuid) -> Result<()> {
    let mut vault = store.vault()?.ok_or(Error::NotFound)?;
    if vault.newer_format {
        return Err(Error::ReadOnly);
    }
    if vault.current().shares.get(&memo_id).is_some_and(|users| users.contains(&user_id)) {
        return Ok(());
    }
    let mut local = vault.current().clone();
    local.shares.entry(memo_id).or_default().insert(user_id);
    vault.local = Some(local);
    store.put_vault(&vault)
}

/// Drops `users` (`None`: everyone) from the memo's share record. Returns whether the vault changed.
pub(crate) fn forget_shares<S: Store + ?Sized>(store: &mut S, memo_id: Uuid, users: Option<&[Uuid]>) -> Result<bool> {
    let Some(mut vault) = store.vault()?.filter(|v| !v.newer_format) else { return Ok(false) };
    let Some(shared) = vault.current().shares.get(&memo_id) else { return Ok(false) };
    let remaining: BTreeSet<Uuid> = match users {
        Some(users) => shared.iter().filter(|u| !users.contains(u)).copied().collect(),
        None => BTreeSet::new(),
    };
    if remaining.len() == shared.len() {
        return Ok(false);
    }
    let mut local = vault.current().clone();
    if remaining.is_empty() {
        local.shares.remove(&memo_id);
    } else {
        local.shares.insert(memo_id, remaining);
    }
    vault.local = Some(local);
    store.put_vault(&vault)?;
    Ok(true)
}

/// Stores a new, unsynced memo owned by the account, with `memo_key[1]`.
pub(crate) fn insert_new_memo<S: Store + ?Sized>(
    store: &mut S,
    account: &UnlockedAccount,
    id: Uuid,
    key: MemoKey,
    payload: MemoPayload,
    now: i64,
) -> Result<()> {
    store.put_memo(&MemoRecord {
        id,
        owner_id: account.user_id,
        server_version: 0,
        key_version: 1,
        base: None,
        local: Some(payload),
        highest_seen_version: 0,
        pending_delete: false,
        conflict: None,
        undecoded: None,
        push_error: None,
        in_flight: None,
        created_at: now,
        updated_at: now,
        updated_by: Some(account.user_id),
    })?;
    store.put_access(&AccessRecord {
        memo_id: id,
        owner_id: account.user_id,
        permission: Permission::Write,
        members: vec![Member {
            user_id: account.user_id,
            username: String::new(),
            public_key: *account.keypair.public_key(),
            permission: Permission::Write,
        }],
        keys: BTreeMap::from([(1, key)]),
    })
}

/// Saves the memo's current content as a new private memo with a new id and key. Attachments
/// whose blobs are cached are re-encrypted under new ids; references to the rest are dropped.
pub(crate) fn restore_as_copy<S: Store + ?Sized>(
    store: &mut S,
    account: &UnlockedAccount,
    memo: &MemoRecord,
    now: i64,
) -> Result<Option<Uuid>> {
    let Some(mut payload) = memo.current().filter(|p| p.is_editable()).cloned() else {
        return Ok(None);
    };
    let copy_id = Uuid::now_v7();
    let mut attachments = Vec::new();
    for old in std::mem::take(&mut payload.attachments) {
        let Some(original) = cached_plaintext(store, &old, AttachmentKind::Original)? else {
            continue;
        };
        let (id, key) = (Uuid::now_v7(), AttachmentKey::generate());
        store.put_blob(
            id,
            AttachmentKind::Original,
            &seal_attachment(&key, id, AttachmentKind::Original, &original),
        )?;
        let thumbnail = match old.thumb {
            Some(_) => cached_plaintext(store, &old, AttachmentKind::Thumbnail)?,
            None => None,
        };
        if let Some(thumbnail) = &thumbnail {
            store.put_blob(
                id,
                AttachmentKind::Thumbnail,
                &seal_attachment(&key, id, AttachmentKind::Thumbnail, thumbnail),
            )?;
        }
        store.put_attachment(&AttachmentRecord {
            id,
            memo_id: copy_id,
            on_server: false,
            uploaded: false,
            thumb_uploaded: false,
            upload_original: true,
            upload_thumbnail: thumbnail.is_some(),
            pending_delete: false,
        })?;
        payload.body = payload.body.replace(&format!("attachment:{}", old.id), &format!("attachment:{id}"));
        attachments.push(AttachmentRef { id, key, thumb: old.thumb.filter(|_| thumbnail.is_some()), ..old });
    }
    payload.attachments = attachments;
    insert_new_memo(store, account, copy_id, MemoKey::generate(), payload, now)?;
    Ok(Some(copy_id))
}

pub(crate) fn cached_plaintext<S: Store + ?Sized>(
    store: &S,
    attachment: &AttachmentRef,
    kind: AttachmentKind,
) -> Result<Option<Vec<u8>>> {
    let Some(envelope) = store.blob(attachment.id, kind)? else {
        return Ok(None);
    };
    Ok(open_attachment(&attachment.key, attachment.id, kind, &envelope).ok())
}

pub(crate) enum PinCheck {
    /// Matches the pinned key.
    Matches,
    /// Was not pinned; it is now (the vault is dirty).
    Pinned,
    /// A different key is pinned. Use that one; raise the alarm.
    Changed(PublicKey),
    /// Not pinned, and its username is pinned under another user id. Do not trust it; raise the alarm.
    NameTaken,
    /// Not pinned, and the vault was written by a newer client, so it cannot be pinned now.
    Unavailable,
}

/// Trust on first use: compares `presented` with the pin for `user_id`, pinning it if there is none.
pub(crate) fn check_pin<S: Store + ?Sized>(
    store: &mut S,
    user_id: Uuid,
    username: &str,
    presented: PublicKey,
    now: i64,
) -> Result<PinCheck> {
    let mut vault = store.vault()?.unwrap_or_else(|| VaultRecord {
        server_version: 0,
        base: VaultPayload::default(),
        local: None,
        newer_format: false,
    });
    match vault.current().contacts.get(&user_id) {
        Some(contact) if contact.public_key == presented => Ok(PinCheck::Matches),
        Some(contact) => Ok(PinCheck::Changed(contact.public_key)),
        None if name_taken(vault.current(), user_id, username) => Ok(PinCheck::NameTaken),
        None if vault.newer_format => Ok(PinCheck::Unavailable),
        None => {
            let mut local = vault.current().clone();
            let contact =
                Contact { username: username.to_owned(), public_key: presented, pinned_at: now, verified: false };
            local.contacts.insert(user_id, contact);
            vault.local = Some(local);
            store.put_vault(&vault)?;
            Ok(PinCheck::Pinned)
        }
    }
}

/// Whether `username` is pinned under another user id. Usernames are unique and never change, so
/// that is another account presented under a known name.
pub(crate) fn name_taken(vault: &VaultPayload, user_id: Uuid, username: &str) -> bool {
    let name = username.trim();
    !name.is_empty() && vault.contacts.iter().any(|(id, c)| *id != user_id && c.username.eq_ignore_ascii_case(name))
}

pub(crate) fn pinned_key<S: Store + ?Sized>(store: &S, user_id: Uuid) -> Result<Option<PublicKey>> {
    Ok(store.vault()?.and_then(|v| v.current().contacts.get(&user_id).map(|c| c.public_key)))
}
