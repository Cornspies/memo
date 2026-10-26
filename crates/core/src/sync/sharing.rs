//! Request builders for sharing and revoking (online-only REST calls, see `docs/api.md#sharing`).

use uuid::Uuid;

use super::{
    ops::{self, PinCheck, transaction},
    store::{MemoRecord, Store},
};
use crate::{
    Error, Result,
    account::UnlockedAccount,
    api::{AddMemberRequest, MemberKey, Permission, RevokeRequest, RevokeResponse, UserInfo, WrappedKey},
    content::{MemoContext, seal_memo},
    keys::MemoKey,
    share::{KeyGrant, wrap_memo_key},
};

/// Body of `POST /memos/{id}/members`: every key version wrapped for `user`. Pins `user`'s key
/// (from `GET /users/lookup`) if it is new, and refuses if it differs from the pin.
pub fn prepare_share<S: Store>(
    store: &mut S,
    account: &UnlockedAccount,
    memo_id: Uuid,
    user: &UserInfo,
    permission: Permission,
    now: i64,
) -> Result<AddMemberRequest> {
    transaction(store, |store| {
        let memo = owned_on_server(store, account, memo_id)?;
        let access = store.access(memo.id)?.ok_or(Error::NotFound)?;
        match ops::check_pin(store, user.user_id, &user.username, user.public_key, now)? {
            PinCheck::Matches | PinCheck::Pinned => {}
            PinCheck::Changed(_) => return Err(Error::KeyChanged(user.user_id)),
            PinCheck::Unavailable => return Err(Error::ReadOnly),
        }
        let keys = access
            .keys
            .iter()
            .map(|(&key_version, key)| {
                let grant = KeyGrant { memo_id, key_version, owner_id: account.user_id, member_id: user.user_id };
                WrappedKey { key_version, wrapped_key: wrap_memo_key(&account.keypair, &user.public_key, &grant, key) }
            })
            .collect();
        Ok(AddMemberRequest { user_id: user.user_id, permission, keys })
    })
}

/// A revocation in progress. Send `request` to `POST /memos/{id}/revoke`, then call
/// [`apply_revoke`]. On `409`, sync and prepare again.
#[derive(Debug)]
pub struct PreparedRevoke {
    pub memo_id: Uuid,
    pub request: RevokeRequest,
    new_key: MemoKey,
}

/// Rotates the memo key away from `user_ids`. The remaining members are taken from this device's
/// view, and each gets the new key wrapped to their **pinned** public key.
pub fn prepare_revoke<S: Store>(
    store: &S,
    account: &UnlockedAccount,
    memo_id: Uuid,
    user_ids: &[Uuid],
) -> Result<PreparedRevoke> {
    let memo = owned_on_server(store, account, memo_id)?;
    let base = memo.base.as_ref().filter(|_| memo.undecoded.is_none()).ok_or(Error::ReadOnly)?;
    let access = store.access(memo_id)?.ok_or(Error::NotFound)?;
    let new_key_version = access.keys.keys().max().copied().unwrap_or(0) + 1;
    let new_key = MemoKey::generate();
    let base_version = memo.server_version;
    let ctx = MemoContext { memo_id, version: base_version + 1, key_version: new_key_version };
    let ciphertext = seal_memo(&new_key, &ctx, base)?;

    let mut remaining: Vec<Uuid> = vec![account.user_id];
    remaining
        .extend(access.members.iter().map(|m| m.user_id).filter(|id| *id != account.user_id && !user_ids.contains(id)));
    let mut keys = Vec::with_capacity(remaining.len());
    for user_id in remaining {
        let public_key = if user_id == account.user_id {
            *account.keypair.public_key()
        } else {
            let pinned = ops::pinned_key(store, user_id)?.ok_or(Error::NotPinned(user_id))?;
            let presented = access.members.iter().find(|m| m.user_id == user_id).map(|m| m.public_key);
            if presented != Some(pinned) {
                return Err(Error::KeyChanged(user_id));
            }
            pinned
        };
        let grant = KeyGrant { memo_id, key_version: new_key_version, owner_id: account.user_id, member_id: user_id };
        keys.push(MemberKey { user_id, wrapped_key: wrap_memo_key(&account.keypair, &public_key, &grant, &new_key) });
    }
    let request = RevokeRequest { user_ids: user_ids.to_vec(), base_version, new_key_version, ciphertext, keys };
    Ok(PreparedRevoke { memo_id, request, new_key })
}

/// Records the rotation the server accepted. Unsynced local edits stay dirty and are pushed
/// under the new key by the next sync.
pub fn apply_revoke<S: Store>(store: &mut S, prepared: PreparedRevoke, response: RevokeResponse) -> Result<()> {
    let PreparedRevoke { memo_id, request, new_key } = prepared;
    if response.version != request.base_version + 1 {
        return Err(Error::Malformed("revoke response version"));
    }
    transaction(store, |store| {
        let mut memo = store.memo(memo_id)?.ok_or(Error::NotFound)?;
        let mut access = store.access(memo_id)?.ok_or(Error::NotFound)?;
        access.keys.insert(request.new_key_version, new_key);
        access.members.retain(|m| !request.user_ids.contains(&m.user_id));
        memo.server_version = response.version;
        memo.key_version = request.new_key_version;
        memo.highest_seen_version = memo.highest_seen_version.max(response.version);
        store.put_access(&access)?;
        store.put_memo(&memo)
    })
}

fn owned_on_server<S: Store>(store: &S, account: &UnlockedAccount, memo_id: Uuid) -> Result<MemoRecord> {
    let memo = store.memo(memo_id)?.ok_or(Error::NotFound)?;
    if memo.owner_id != account.user_id {
        return Err(Error::NotOwner);
    }
    if memo.server_version == 0 {
        // Not on the server yet: sync first.
        return Err(Error::NotFound);
    }
    Ok(memo)
}
