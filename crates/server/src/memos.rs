//! Sharing and revisions (`docs/api.md#sharing`, `#revisions`). Only the owner shares, changes
//! permissions or revokes; the server checks that the wrapped keys cover exactly who they must.

use std::collections::BTreeSet;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use memo_core::{
    WRAPPED_KEY_LEN,
    api::{AddMemberRequest, Permission, Revision, RevisionInfo, RevokeRequest, RevokeResponse, UpdateMemberRequest},
};
use rusqlite::{OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::{
    auth::Session,
    db::{self, Entity, RowExt, now_ms},
    error::{ApiError, ApiResult},
    state::App,
    sync::{insert_revision, permission_str, valid_ciphertext, visible_memo},
};

/// The memo's owner, version and key version, if the caller owns it.
fn owned(tx: &Transaction, memo_id: Uuid, user: Uuid) -> ApiResult<(u64, u32)> {
    let (owner, version, key_version, _) = visible_memo(tx, memo_id, user)?;
    if owner != user {
        return Err(ApiError::forbidden("only the owner can do this"));
    }
    Ok((version, key_version))
}

pub async fn add_member(
    State(app): State<App>,
    session: Session,
    Path(memo_id): Path<Uuid>,
    Json(request): Json<AddMemberRequest>,
) -> ApiResult<StatusCode> {
    app.db
        .write(move |tx| {
            owned(tx, memo_id, session.user_id)?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM users WHERE id = ?1 AND disabled = 0)",
                [request.user_id.as_bytes()],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(ApiError::not_found());
            }
            if db::permission(tx, memo_id, request.user_id)?.is_some() {
                return Err(ApiError::conflict("already a member"));
            }
            let existing: BTreeSet<u32> = tx
                .prepare_cached("SELECT key_version FROM memo_keys WHERE memo_id = ?1 AND user_id = ?2")?
                .query_map(params![memo_id.as_bytes(), session.user_id.as_bytes()], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let given: BTreeSet<u32> = request.keys.iter().map(|k| k.key_version).collect();
            if given != existing
                || given.len() != request.keys.len()
                || request.keys.iter().any(|k| k.wrapped_key.len() != WRAPPED_KEY_LEN)
            {
                return Err(ApiError::incomplete_keys());
            }
            tx.execute(
                "INSERT INTO memo_members (memo_id, user_id, permission, added_at) VALUES (?1, ?2, ?3, ?4)",
                params![memo_id.as_bytes(), request.user_id.as_bytes(), permission_str(request.permission), now_ms()],
            )?;
            for key in &request.keys {
                tx.execute(
                    "INSERT INTO memo_keys (memo_id, user_id, key_version, wrapped_key) VALUES (?1, ?2, ?3, ?4)",
                    params![memo_id.as_bytes(), request.user_id.as_bytes(), key.key_version, key.wrapped_key],
                )?;
            }
            // Everyone's member list changed; the new member also needs the memo and its images.
            db::log_for_members(tx, memo_id, Entity::Access, memo_id)?;
            db::log_change(tx, request.user_id, Entity::Memo, memo_id)?;
            let attachments: Vec<Uuid> = tx
                .prepare_cached("SELECT id FROM attachments WHERE memo_id = ?1 AND deleted_at IS NULL")?
                .query_map([memo_id.as_bytes()], |r| r.uuid(0))?
                .collect::<Result<_, _>>()?;
            for attachment in attachments {
                db::log_change(tx, request.user_id, Entity::Attachment, attachment)?;
            }
            Ok(StatusCode::CREATED)
        })
        .await
}

pub async fn update_member(
    State(app): State<App>,
    session: Session,
    Path((memo_id, user_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateMemberRequest>,
) -> ApiResult<StatusCode> {
    app.db
        .write(move |tx| {
            owned(tx, memo_id, session.user_id)?;
            if user_id == session.user_id {
                return Err(ApiError::forbidden("the owner always has write access"));
            }
            let updated = tx.execute(
                "UPDATE memo_members SET permission = ?1 WHERE memo_id = ?2 AND user_id = ?3",
                params![permission_str(request.permission), memo_id.as_bytes(), user_id.as_bytes()],
            )?;
            if updated == 0 {
                return Err(ApiError::not_found());
            }
            db::log_for_members(tx, memo_id, Entity::Access, memo_id)?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

pub async fn revoke(
    State(app): State<App>,
    session: Session,
    Path(memo_id): Path<Uuid>,
    Json(request): Json<RevokeRequest>,
) -> ApiResult<Json<RevokeResponse>> {
    let limits = app.config.limits.clone();
    app.db
        .write(move |tx| {
            let owner = session.user_id;
            let (version, key_version) = owned(tx, memo_id, owner)?;
            if request.base_version != version {
                return Err(ApiError::conflict("stale base_version"));
            }
            if request.new_key_version != key_version + 1 {
                return Err(ApiError::bad_request("new_key_version must be the current key version + 1"));
            }
            let everyone = db::members_of(tx, memo_id)?;
            let removed: BTreeSet<Uuid> = request.user_ids.iter().copied().collect();
            if removed.is_empty() || removed.contains(&owner) || !removed.iter().all(|u| everyone.contains(u)) {
                return Err(ApiError::bad_request("user_ids must be members other than the owner"));
            }
            let remaining: BTreeSet<Uuid> = everyone.iter().copied().filter(|u| !removed.contains(u)).collect();
            let covered: BTreeSet<Uuid> = request.keys.iter().map(|k| k.user_id).collect();
            if covered != remaining
                || covered.len() != request.keys.len()
                || request.keys.iter().any(|k| k.wrapped_key.len() != WRAPPED_KEY_LEN)
            {
                return Err(ApiError::incomplete_keys());
            }
            if request.ciphertext.len() as u64 > limits.max_memo.0 {
                return Err(ApiError::too_large());
            }
            if !valid_ciphertext(&limits, &request.ciphertext) {
                return Err(ApiError::bad_request("ciphertext is not an envelope"));
            }
            if !db::fits_quota(tx, owner, request.ciphertext.len() as u64, limits.default_quota.0)? {
                return Err(ApiError::quota_exceeded());
            }
            let now = now_ms();
            let new_version = version + 1;
            tx.execute(
                "UPDATE memos SET version = ?1, key_version = ?2, ciphertext = ?3, updated_at = ?4, updated_by = ?5 WHERE id = ?6",
                params![new_version, request.new_key_version, request.ciphertext, now, owner.as_bytes(), memo_id.as_bytes()],
            )?;
            insert_revision(tx, memo_id, new_version, request.new_key_version, &request.ciphertext, owner, now)?;
            for user in &removed {
                tx.execute("DELETE FROM memo_members WHERE memo_id = ?1 AND user_id = ?2", params![memo_id.as_bytes(), user.as_bytes()])?;
                db::forget_memo(tx, memo_id, *user)?;
            }
            for key in &request.keys {
                tx.execute(
                    "INSERT INTO memo_keys (memo_id, user_id, key_version, wrapped_key) VALUES (?1, ?2, ?3, ?4)",
                    params![memo_id.as_bytes(), key.user_id.as_bytes(), request.new_key_version, key.wrapped_key],
                )?;
            }
            // Removed users see `revoked: true`; the others get the new key and version.
            for user in &everyone {
                db::log_change(tx, *user, Entity::Access, memo_id)?;
            }
            db::log_for_members(tx, memo_id, Entity::Memo, memo_id)?;
            Ok(Json(RevokeResponse { version: new_version }))
        })
        .await
}

pub async fn leave(State(app): State<App>, session: Session, Path(memo_id): Path<Uuid>) -> ApiResult<StatusCode> {
    app.db
        .write(move |tx| {
            let (owner, ..) = visible_memo(tx, memo_id, session.user_id)?;
            if owner == session.user_id {
                return Err(ApiError::forbidden("the owner cannot leave; delete the memo instead"));
            }
            tx.execute(
                "DELETE FROM memo_members WHERE memo_id = ?1 AND user_id = ?2",
                params![memo_id.as_bytes(), session.user_id.as_bytes()],
            )?;
            db::forget_memo(tx, memo_id, session.user_id)?;
            db::log_change(tx, session.user_id, Entity::Access, memo_id)?;
            db::log_for_members(tx, memo_id, Entity::Access, memo_id)?;
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

pub async fn revisions(
    State(app): State<App>,
    session: Session,
    Path(memo_id): Path<Uuid>,
) -> ApiResult<Json<Vec<RevisionInfo>>> {
    app.db
        .read(move |conn| {
            visible_memo(conn, memo_id, session.user_id)?;
            let revisions = conn
                .prepare_cached(
                    "SELECT version, key_version, updated_by, updated_at, LENGTH(ciphertext) FROM memo_revisions
                     WHERE memo_id = ?1 ORDER BY version DESC",
                )?
                .query_map([memo_id.as_bytes()], |r| {
                    Ok(RevisionInfo {
                        version: r.get(0)?,
                        key_version: r.get(1)?,
                        updated_by: r.opt_uuid(2)?,
                        updated_at: r.get(3)?,
                        size: r.get(4)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(Json(revisions))
        })
        .await
}

pub async fn revision(
    State(app): State<App>,
    session: Session,
    Path((memo_id, version)): Path<(Uuid, u64)>,
) -> ApiResult<Json<Revision>> {
    app.db
        .read(move |conn| {
            visible_memo(conn, memo_id, session.user_id)?;
            conn.query_row(
                "SELECT key_version, ciphertext FROM memo_revisions WHERE memo_id = ?1 AND version = ?2",
                params![memo_id.as_bytes(), version],
                |r| Ok(Revision { version, key_version: r.get(0)?, ciphertext: r.get(1)? }),
            )
            .optional()?
            .map(Json)
            .ok_or_else(ApiError::not_found)
        })
        .await
}

/// Members can read; write-only checks happen where needed.
pub fn require_write(permission: Permission) -> ApiResult<()> {
    match permission {
        Permission::Write => Ok(()),
        Permission::Read => Err(ApiError::forbidden("read-only access")),
    }
}
