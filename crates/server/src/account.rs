//! `/me`, `/devices` and `/users` (`docs/api.md#account`, `#users`).

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use memo_core::{
    PublicKey,
    api::{DeleteAccountRequest, Device, Me, Role, UserInfo},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    auth::{Session, parse_kind, verify_current_auth_key},
    db::{self, Entity, RowExt, now_ms},
    error::{ApiError, ApiResult},
    state::App,
    sync::delete_memo,
};

pub async fn me(State(app): State<App>, session: Session) -> ApiResult<Json<Me>> {
    let default_quota = app.config.limits.default_quota.0;
    app.db
        .read(move |conn| {
            let user = session.user_id;
            let (username, role, public_key, created_at) = conn.query_row(
                "SELECT username, role, public_key, created_at FROM users WHERE id = ?1",
                [user.as_bytes()],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, [u8; 32]>(2)?, r.get(3)?)),
            )?;
            Ok(Json(Me {
                user_id: user,
                username,
                role: if role == "admin" { Role::Admin } else { Role::User },
                public_key: PublicKey::from_bytes(&public_key).map_err(ApiError::internal)?,
                quota_bytes: db::quota_bytes(conn, user, default_quota)?,
                used_bytes: db::used_bytes(conn, user)?,
                created_at,
            }))
        })
        .await
}

pub async fn devices(State(app): State<App>, session: Session) -> ApiResult<Json<Vec<Device>>> {
    app.db
        .read(move |conn| {
            let devices = conn
                .prepare_cached(
                    "SELECT id, name, kind, created_at, last_seen_at FROM devices
                     WHERE user_id = ?1 AND revoked_at IS NULL ORDER BY created_at",
                )?
                .query_map([session.user_id.as_bytes()], |r| {
                    Ok((r.uuid(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get(3)?, r.get(4)?))
                })?
                .map(|row| {
                    let (id, name, kind, created_at, last_seen_at) = row?;
                    let kind = parse_kind(&kind).ok_or_else(|| ApiError::internal("device kind"))?;
                    Ok(Device { id, name, kind, created_at, last_seen_at, current: id == session.device_id })
                })
                .collect::<ApiResult<_>>()?;
            Ok(Json(devices))
        })
        .await
}

pub async fn delete_device(State(app): State<App>, session: Session, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    app.db
        .write(move |tx| {
            let revoked = tx.execute(
                "UPDATE devices SET revoked_at = ?1 WHERE id = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                params![now_ms(), id.as_bytes(), session.user_id.as_bytes()],
            )?;
            if revoked == 0 {
                return Err(ApiError::not_found());
            }
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

pub async fn delete_me(
    State(app): State<App>,
    session: Session,
    Json(request): Json<DeleteAccountRequest>,
) -> ApiResult<StatusCode> {
    verify_current_auth_key(&app, session.user_id, *request.auth_key.as_bytes()).await?;
    app.db.write(move |tx| delete_user(tx, session.user_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Deletes owned memos (members lose access), leaves shared ones, deletes the user's uploads,
/// then the user. Blob files are removed by the next garbage collection.
pub fn delete_user(tx: &Transaction, user: Uuid) -> ApiResult<()> {
    let owned: Vec<Uuid> = tx
        .prepare_cached("SELECT id FROM memos WHERE owner_id = ?1 AND deleted = 0")?
        .query_map([user.as_bytes()], |r| r.uuid(0))?
        .collect::<Result<_, _>>()?;
    for memo_id in owned {
        delete_memo(tx, memo_id)?;
    }
    let shared: Vec<Uuid> = tx
        .prepare_cached("SELECT memo_id FROM memo_members WHERE user_id = ?1")?
        .query_map([user.as_bytes()], |r| r.uuid(0))?
        .collect::<Result<_, _>>()?;
    for memo_id in shared {
        tx.execute(
            "DELETE FROM memo_members WHERE memo_id = ?1 AND user_id = ?2",
            params![memo_id.as_bytes(), user.as_bytes()],
        )?;
        db::log_for_members(tx, memo_id, Entity::Access, memo_id)?;
    }
    let uploads: Vec<(Uuid, Uuid)> = tx
        .prepare_cached("SELECT id, memo_id FROM attachments WHERE uploader_id = ?1 AND deleted_at IS NULL")?
        .query_map([user.as_bytes()], |r| Ok((r.uuid(0)?, r.uuid(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, memo_id) in uploads {
        tx.execute("UPDATE attachments SET deleted_at = ?1 WHERE id = ?2", params![now_ms(), id.as_bytes()])?;
        db::log_for_members(tx, memo_id, Entity::Attachment, id)?;
    }
    tx.execute("DELETE FROM change_log WHERE user_id = ?1", [user.as_bytes()])?;
    tx.execute("DELETE FROM users WHERE id = ?1", [user.as_bytes()])?;
    Ok(())
}

#[derive(Deserialize)]
pub struct LookupQuery {
    username: String,
}

pub async fn lookup(
    State(app): State<App>,
    _session: Session,
    Query(query): Query<LookupQuery>,
) -> ApiResult<Json<UserInfo>> {
    let username = query.username.trim().to_lowercase();
    app.db.read(move |conn| user_info(conn, "username = ?1", &username)).await.map(Json)
}

pub async fn user(State(app): State<App>, _session: Session, Path(id): Path<Uuid>) -> ApiResult<Json<UserInfo>> {
    app.db.read(move |conn| user_info(conn, "id = ?1", id.as_bytes())).await.map(Json)
}

fn user_info(conn: &Connection, condition: &str, value: &dyn rusqlite::ToSql) -> ApiResult<UserInfo> {
    let row = conn
        .query_row(
            &format!("SELECT id, username, public_key FROM users WHERE {condition} AND disabled = 0"),
            [value],
            |r| Ok((r.uuid(0)?, r.get::<_, String>(1)?, r.get::<_, [u8; 32]>(2)?)),
        )
        .optional()?;
    let (user_id, username, public_key) = row.ok_or_else(ApiError::not_found)?;
    Ok(UserInfo { user_id, username, public_key: PublicKey::from_bytes(&public_key).map_err(ApiError::internal)? })
}
