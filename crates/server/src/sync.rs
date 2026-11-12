//! `POST /api/v1/sync`. See `docs/sync.md`; the in-memory server in `memo-core`'s sync tests
//! implements the same rules.

use axum::{Json, extract::State};
use memo_core::{
    ENVELOPE_OVERHEAD, PublicKey, WRAPPED_KEY_LEN,
    api::{
        AccessChange, AttachmentChange, Change, MAX_PUSH_OPS, Member, MemoChange, OpResult, OpStatus, Permission,
        PushOp, StateChange, SyncRequest, SyncResponse, VaultChange, WrappedKey,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use uuid::Uuid;

use crate::{
    auth::Session,
    config::Limits,
    db::{self, Entity, Quota, RowExt, now_ms},
    error::{ApiError, ApiResult},
    state::App,
};

const PAGE_ENTITIES: usize = 500;
const PAGE_BYTES: usize = 4 << 20;

pub async fn sync(
    State(app): State<App>,
    session: Session,
    Json(request): Json<SyncRequest>,
) -> ApiResult<Json<SyncResponse>> {
    if request.push.len() > MAX_PUSH_OPS {
        return Err(ApiError::bad_request(format!("at most {MAX_PUSH_OPS} ops per request")));
    }
    let user = session.user_id;
    if request.push.is_empty() {
        // Nothing to write: a reader does it, and the writer stays free for others.
        return app.db.read(move |conn| pull_only(conn, user, &request)).await.map(Json);
    }
    let limits = app.config.limits.clone();
    app.db.write(move |tx| run(tx, &limits, user, request)).await.map(Json)
}

fn run(tx: &Transaction, limits: &Limits, user: Uuid, request: SyncRequest) -> ApiResult<SyncResponse> {
    let (epoch, reset) = epoch_and_reset(tx, user, &request)?;
    if reset {
        return Ok(reset_response(epoch));
    }
    let mut quota = Quota::default();
    let results =
        request.push.iter().map(|op| apply(tx, limits, &mut quota, user, op)).collect::<ApiResult<Vec<_>>>()?;
    pull(tx, user, request.cursor, epoch, results)
}

fn pull_only(conn: &Connection, user: Uuid, request: &SyncRequest) -> ApiResult<SyncResponse> {
    // One snapshot for all the reads below.
    let snapshot = conn.unchecked_transaction()?;
    let (epoch, reset) = epoch_and_reset(&snapshot, user, request)?;
    if reset {
        return Ok(reset_response(epoch));
    }
    pull(&snapshot, user, request.cursor, epoch, Vec::new())
}

/// The epoch, and whether the client's epoch or cursor no longer fit this database.
fn epoch_and_reset(conn: &Connection, user: Uuid, request: &SyncRequest) -> ApiResult<(String, bool)> {
    let epoch = db::epoch(conn)?;
    let purged_through: u64 =
        conn.query_row("SELECT purged_through FROM users WHERE id = ?1", [user.as_bytes()], |r| r.get(0))?;
    let max_seq: u64 =
        conn.query_row("SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'change_log'), 0)", [], |r| {
            r.get(0)
        })?;
    let cursor = request.cursor;
    let reset = request.epoch.as_ref().is_some_and(|e| *e != epoch)
        || (cursor > 0 && cursor < purged_through)
        || cursor > max_seq;
    Ok((epoch, reset))
}

fn reset_response(epoch: String) -> SyncResponse {
    SyncResponse { results: vec![], changes: vec![], cursor: 0, epoch, more: false, reset: true }
}

/// The page of changes after `cursor`.
fn pull(conn: &Connection, user: Uuid, cursor: u64, epoch: String, results: Vec<OpResult>) -> ApiResult<SyncResponse> {
    let mut changes = Vec::new();
    // A full sync starts with the vault, so clients know their pins before verifying keys.
    if cursor == 0
        && let Some(seq) = conn
            .query_row(
                "SELECT seq FROM change_log WHERE user_id = ?1 AND entity_type = 'vault'",
                [user.as_bytes()],
                |r| r.get::<_, u64>(0),
            )
            .optional()?
    {
        changes.extend(render(conn, user, seq, Entity::Vault, user)?);
    }
    let rows: Vec<(u64, String, Uuid)> = conn
        .prepare_cached(
            "SELECT seq, entity_type, entity_id FROM change_log WHERE user_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?
        .query_map(params![user.as_bytes(), cursor, PAGE_ENTITIES as i64 + 1], |r| {
            Ok((r.get(0)?, r.get(1)?, r.uuid(2)?))
        })?
        .collect::<Result<_, _>>()?;
    // A full batch of rows: there may be more behind it, whatever of it ends up on the page.
    let mut more = rows.len() > PAGE_ENTITIES;
    let (mut page_cursor, mut count, mut bytes) = (cursor, 0, 0);
    for (seq, entity, id) in rows {
        if count == PAGE_ENTITIES || (count > 0 && bytes >= PAGE_BYTES) {
            more = true;
            break;
        }
        let entity = Entity::parse(&entity).ok_or_else(|| ApiError::internal(format!("entity type {entity}")))?;
        page_cursor = seq;
        // Nothing to send for an entity the user may no longer see.
        let Some(change) = render(conn, user, seq, entity, id)? else { continue };
        bytes += change_size(&change);
        changes.push(change);
        count += 1;
    }
    Ok(SyncResponse { results, changes, cursor: page_cursor, epoch, more, reset: false })
}

fn change_size(change: &Change) -> usize {
    match change {
        Change::Memo(m) => m.ciphertext.as_ref().map_or(0, Vec::len) + 200,
        Change::Vault(v) => v.ciphertext.len() + 100,
        Change::Access(a) => a.keys.len() * (WRAPPED_KEY_LEN + 20) + a.members.len() * 120 + 100,
        Change::Attachment(_) | Change::State(_) => 150,
    }
}

// ---- Rendering current state ----

/// The current state of an entity in the user's log. `None` for a live memo, or an attachment of
/// one, that the user is not a member of: nothing of it is sent.
pub fn render(conn: &Connection, user: Uuid, seq: u64, entity: Entity, id: Uuid) -> ApiResult<Option<Change>> {
    Ok(Some(match entity {
        Entity::Memo => match render_memo(conn, user, seq, id)? {
            Some(memo) => Change::Memo(memo),
            None => return Ok(None),
        },
        Entity::Access => Change::Access(render_access(conn, user, seq, id)?),
        Entity::Attachment => {
            let row = conn
                .prepare_cached(
                    "SELECT memo_id, size, thumb_size, deleted_at IS NOT NULL FROM attachments WHERE id = ?1",
                )?
                .query_row([id.as_bytes()], |r| {
                    Ok((r.uuid(0)?, r.get::<_, Option<u64>>(1)?, r.get::<_, Option<u64>>(2)?, r.get::<_, bool>(3)?))
                })
                .optional()?;
            // The row is gone once its memo's tombstone was purged: still a deletion.
            let (memo_id, size, thumb_size, deleted) = row.unwrap_or((Uuid::nil(), None, None, true));
            if !deleted && db::permission(conn, memo_id, user)?.is_none() {
                return Ok(None);
            }
            Change::Attachment(AttachmentChange {
                seq,
                id,
                memo_id,
                size,
                thumb_size,
                uploaded: size.is_some(),
                thumb_uploaded: thumb_size.is_some(),
                deleted,
            })
        }
        Entity::State => {
            let (pinned, archived) = conn
                .prepare_cached("SELECT pinned, archived FROM memo_state WHERE memo_id = ?1 AND user_id = ?2")?
                .query_row(params![id.as_bytes(), user.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()?
                .unwrap_or((false, false));
            Change::State(StateChange { seq, memo_id: id, pinned, archived })
        }
        Entity::Vault => {
            let (version, ciphertext) =
                conn.query_row("SELECT version, ciphertext FROM vaults WHERE user_id = ?1", [user.as_bytes()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
            Change::Vault(VaultChange { seq, version, ciphertext })
        }
    }))
}

fn render_memo(conn: &Connection, user: Uuid, seq: u64, id: Uuid) -> ApiResult<Option<MemoChange>> {
    let row = conn
        .prepare_cached(
            "SELECT owner_id, version, key_version, ciphertext, deleted, created_at, updated_at, updated_by FROM memos WHERE id = ?1",
        )?
        .query_row([id.as_bytes()], |r| {
            Ok(MemoChange {
                seq,
                id,
                owner_id: r.uuid(0)?,
                version: r.get(1)?,
                key_version: r.get(2)?,
                ciphertext: r.get(3)?,
                deleted: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
                updated_by: r.opt_uuid(7)?,
            })
        })
        .optional()?;
    // The row is gone when its owner's account was deleted.
    let memo = row.unwrap_or(MemoChange {
        seq,
        id,
        owner_id: Uuid::nil(),
        version: 0,
        key_version: 0,
        ciphertext: None,
        deleted: true,
        created_at: 0,
        updated_at: 0,
        updated_by: None,
    });
    // A tombstone is for everyone who had the memo. The memo itself is for its members only.
    if !memo.deleted && db::permission(conn, id, user)?.is_none() {
        return Ok(None);
    }
    Ok(Some(memo))
}

fn render_access(conn: &Connection, user: Uuid, seq: u64, memo_id: Uuid) -> ApiResult<AccessChange> {
    let revoked =
        AccessChange { seq, memo_id, revoked: true, owner_id: None, permission: None, keys: vec![], members: vec![] };
    let Some(permission) = db::permission(conn, memo_id, user)? else { return Ok(revoked) };
    let owner_id = conn.query_row("SELECT owner_id FROM memos WHERE id = ?1", [memo_id.as_bytes()], |r| r.uuid(0))?;
    let keys = conn
        .prepare_cached(
            "SELECT key_version, wrapped_key FROM memo_keys WHERE memo_id = ?1 AND user_id = ?2 ORDER BY key_version",
        )?
        .query_map(params![memo_id.as_bytes(), user.as_bytes()], |r| {
            Ok(WrappedKey { key_version: r.get(0)?, wrapped_key: r.get(1)? })
        })?
        .collect::<Result<_, _>>()?;
    let members = conn
        .prepare_cached(
            "SELECT m.user_id, u.username, u.public_key, m.permission FROM memo_members m JOIN users u ON u.id = m.user_id
             WHERE m.memo_id = ?1 ORDER BY m.added_at",
        )?
        .query_map([memo_id.as_bytes()], |r| {
            Ok((r.uuid(0)?, r.get::<_, String>(1)?, r.get::<_, [u8; 32]>(2)?, r.get::<_, String>(3)?))
        })?
        .map(|row| {
            let (user_id, username, public_key, permission) = row?;
            let public_key = PublicKey::from_bytes(&public_key).map_err(ApiError::internal)?;
            Ok(Member { user_id, username, public_key, permission: parse_permission(&permission)? })
        })
        .collect::<ApiResult<_>>()?;
    Ok(AccessChange {
        seq,
        memo_id,
        revoked: false,
        owner_id: Some(owner_id),
        permission: Some(parse_permission(&permission)?),
        keys,
        members,
    })
}

pub fn parse_permission(text: &str) -> ApiResult<Permission> {
    match text {
        "read" => Ok(Permission::Read),
        "write" => Ok(Permission::Write),
        _ => Err(ApiError::internal(format!("permission {text}"))),
    }
}

pub fn permission_str(permission: Permission) -> &'static str {
    match permission {
        Permission::Read => "read",
        Permission::Write => "write",
    }
}

// ---- Push ops ----

fn status(status: OpStatus) -> OpResult {
    OpResult::status(status)
}

fn ok(version: Option<u64>) -> OpResult {
    OpResult { status: OpStatus::Ok, version, current_version: None }
}

fn conflict(current: u64) -> OpResult {
    OpResult { status: OpStatus::Conflict, version: None, current_version: Some(current) }
}

struct MemoRow {
    owner: Uuid,
    version: u64,
    key_version: u32,
    deleted: bool,
}

pub(crate) fn memo_row(conn: &Connection, id: Uuid) -> rusqlite::Result<Option<(Uuid, u64, u32, bool)>> {
    conn.prepare_cached("SELECT owner_id, version, key_version, deleted FROM memos WHERE id = ?1")?
        .query_row([id.as_bytes()], |r| Ok((r.uuid(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()
}

fn load_memo(conn: &Connection, id: Uuid) -> rusqlite::Result<Option<MemoRow>> {
    Ok(memo_row(conn, id)?.map(|(owner, version, key_version, deleted)| MemoRow {
        owner,
        version,
        key_version,
        deleted,
    }))
}

fn has_logged(conn: &Connection, user: Uuid, entity: Entity, id: Uuid) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM change_log WHERE user_id = ?1 AND entity_type = ?2 AND entity_id = ?3)",
        params![user.as_bytes(), entity.as_str(), id.as_bytes()],
        |r| r.get(0),
    )
}

pub(crate) fn valid_ciphertext(limits: &Limits, ciphertext: &[u8]) -> bool {
    ciphertext.len() > ENVELOPE_OVERHEAD && ciphertext.len() as u64 <= limits.max_memo.0
}

fn apply(tx: &Transaction, limits: &Limits, quota: &mut Quota, user: Uuid, op: &PushOp) -> ApiResult<OpResult> {
    let now = now_ms();
    Ok(match op {
        PushOp::MemoPut { id, base_version, key_version, ciphertext, wrapped_key } => {
            let id = *id;
            let Some(memo) = load_memo(tx, id)? else {
                if *base_version != 0 {
                    return Ok(status(OpStatus::NotFound));
                }
                let valid_key = wrapped_key.as_ref().is_some_and(|k| k.len() == WRAPPED_KEY_LEN);
                if *key_version != 1 || !valid_key || !valid_ciphertext(limits, ciphertext) || id.get_version_num() != 7
                {
                    return Ok(status(OpStatus::Invalid));
                }
                if !quota.take(tx, user, ciphertext.len() as u64, limits.default_quota.0)? {
                    return Ok(status(OpStatus::QuotaExceeded));
                }
                tx.prepare_cached(
                    "INSERT INTO memos (id, owner_id, version, key_version, ciphertext, created_at, updated_at, updated_by)
                     VALUES (?1, ?2, 1, 1, ?3, ?4, ?4, ?2)",
                )?
                .execute(params![id.as_bytes(), user.as_bytes(), ciphertext, now])?;
                insert_revision(tx, id, 1, 1, ciphertext, user, now)?;
                tx.prepare_cached(
                    "INSERT INTO memo_members (memo_id, user_id, permission, added_at) VALUES (?1, ?2, 'write', ?3)",
                )?
                .execute(params![id.as_bytes(), user.as_bytes(), now])?;
                tx.prepare_cached(
                    "INSERT INTO memo_keys (memo_id, user_id, key_version, wrapped_key) VALUES (?1, ?2, 1, ?3)",
                )?
                .execute(params![id.as_bytes(), user.as_bytes(), wrapped_key])?;
                db::log_change(tx, user, Entity::Access, id)?;
                db::log_change(tx, user, Entity::Memo, id)?;
                return Ok(ok(Some(1)));
            };
            if memo.deleted {
                let seen = has_logged(tx, user, Entity::Memo, id)?;
                return Ok(status(if seen { OpStatus::Deleted } else { OpStatus::NotFound }));
            }
            let Some(permission) = db::permission(tx, id, user)? else {
                return Ok(status(if *base_version == 0 { OpStatus::Invalid } else { OpStatus::NotFound }));
            };
            if *base_version == 0 {
                return Ok(conflict(memo.version));
            }
            if permission != "write" {
                return Ok(status(OpStatus::Forbidden));
            }
            if *base_version != memo.version || *key_version != memo.key_version {
                return Ok(conflict(memo.version));
            }
            if !valid_ciphertext(limits, ciphertext) {
                return Ok(status(OpStatus::Invalid));
            }
            if !quota.take(tx, memo.owner, ciphertext.len() as u64, limits.default_quota.0)? {
                return Ok(status(OpStatus::QuotaExceeded));
            }
            let version = memo.version + 1;
            tx.prepare_cached(
                "UPDATE memos SET version = ?1, ciphertext = ?2, updated_at = ?3, updated_by = ?4 WHERE id = ?5",
            )?
            .execute(params![version, ciphertext, now, user.as_bytes(), id.as_bytes()])?;
            insert_revision(tx, id, version, memo.key_version, ciphertext, user, now)?;
            db::log_for_members(tx, id, Entity::Memo, id)?;
            ok(Some(version))
        }
        PushOp::MemoDelete { id, base_version } => {
            let id = *id;
            let Some(memo) = load_memo(tx, id)? else { return Ok(status(OpStatus::NotFound)) };
            if memo.deleted {
                let seen = has_logged(tx, user, Entity::Memo, id)?;
                return Ok(if seen { ok(None) } else { status(OpStatus::NotFound) });
            }
            if db::permission(tx, id, user)?.is_none() {
                return Ok(status(OpStatus::NotFound));
            }
            if memo.owner != user {
                return Ok(status(OpStatus::Forbidden));
            }
            if *base_version != memo.version {
                return Ok(conflict(memo.version));
            }
            delete_memo(tx, id)?;
            quota.forget(memo.owner);
            ok(None)
        }
        PushOp::StatePut { memo_id, pinned, archived } => {
            if db::permission(tx, *memo_id, user)?.is_none() {
                return Ok(status(OpStatus::NotFound));
            }
            tx.prepare_cached(
                "INSERT INTO memo_state (memo_id, user_id, pinned, archived, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (memo_id, user_id) DO UPDATE SET pinned = ?3, archived = ?4, updated_at = ?5",
            )?
            .execute(params![memo_id.as_bytes(), user.as_bytes(), pinned, archived, now])?;
            db::log_change(tx, user, Entity::State, *memo_id)?;
            ok(None)
        }
        PushOp::AttachmentDelete { id } => {
            let row = tx
                .query_row(
                    "SELECT memo_id, deleted_at IS NOT NULL FROM attachments WHERE id = ?1",
                    [id.as_bytes()],
                    |r| Ok((r.uuid(0)?, r.get::<_, bool>(1)?)),
                )
                .optional()?;
            let Some((memo_id, deleted)) = row else { return Ok(ok(None)) };
            match db::permission(tx, memo_id, user)?.as_deref() {
                None => return Ok(status(OpStatus::NotFound)),
                Some("write") => {}
                Some(_) => return Ok(status(OpStatus::Forbidden)),
            }
            if !deleted {
                tx.execute("UPDATE attachments SET deleted_at = ?1 WHERE id = ?2", params![now, id.as_bytes()])?;
                db::log_for_members(tx, memo_id, Entity::Attachment, *id)?;
            }
            ok(None)
        }
        PushOp::VaultPut { base_version, ciphertext } => {
            let version: u64 =
                tx.query_row("SELECT version FROM vaults WHERE user_id = ?1", [user.as_bytes()], |r| r.get(0))?;
            if *base_version != version {
                return Ok(conflict(version));
            }
            if !valid_ciphertext(limits, ciphertext) {
                return Ok(status(OpStatus::Invalid));
            }
            tx.execute(
                "UPDATE vaults SET version = ?1, ciphertext = ?2, updated_at = ?3 WHERE user_id = ?4",
                params![version + 1, ciphertext, now, user.as_bytes()],
            )?;
            db::log_change(tx, user, Entity::Vault, user)?;
            ok(Some(version + 1))
        }
    })
}

pub fn insert_revision(
    tx: &Transaction,
    memo_id: Uuid,
    version: u64,
    key_version: u32,
    ciphertext: &[u8],
    user: Uuid,
    now: i64,
) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "INSERT INTO memo_revisions (memo_id, version, key_version, ciphertext, updated_by, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![memo_id.as_bytes(), version, key_version, ciphertext, user.as_bytes(), now])?;
    Ok(())
}

/// Tombstones a memo: drops content, revisions and access, deletes its attachments, and tells
/// every former member.
pub fn delete_memo(tx: &Transaction, memo_id: Uuid) -> ApiResult<()> {
    let now = now_ms();
    let former = db::members_of(tx, memo_id)?;
    let attachments: Vec<Uuid> = tx
        .prepare_cached("SELECT id FROM attachments WHERE memo_id = ?1 AND deleted_at IS NULL")?
        .query_map([memo_id.as_bytes()], |r| r.uuid(0))?
        .collect::<Result<_, _>>()?;
    tx.execute(
        "UPDATE attachments SET deleted_at = ?1 WHERE memo_id = ?2 AND deleted_at IS NULL",
        params![now, memo_id.as_bytes()],
    )?;
    tx.execute(
        "UPDATE memos SET ciphertext = NULL, deleted = 1, deleted_at = ?1, updated_at = ?1 WHERE id = ?2",
        params![now, memo_id.as_bytes()],
    )?;
    tx.execute("DELETE FROM memo_revisions WHERE memo_id = ?1", [memo_id.as_bytes()])?;
    tx.execute("DELETE FROM memo_members WHERE memo_id = ?1", [memo_id.as_bytes()])?;
    for user in former {
        for attachment in &attachments {
            db::log_change(tx, user, Entity::Attachment, *attachment)?;
        }
        db::log_change(tx, user, Entity::Memo, memo_id)?;
    }
    Ok(())
}

/// Version and key version of a memo the caller can see, for the REST endpoints.
pub(crate) fn visible_memo(conn: &Connection, memo_id: Uuid, user: Uuid) -> ApiResult<(Uuid, u64, u32, Permission)> {
    let (owner, version, key_version, deleted) = memo_row(conn, memo_id)?.ok_or_else(ApiError::not_found)?;
    let permission = db::permission(conn, memo_id, user)?.ok_or_else(ApiError::not_found)?;
    if deleted {
        return Err(ApiError::not_found());
    }
    Ok((owner, version, key_version, parse_permission(&permission)?))
}
