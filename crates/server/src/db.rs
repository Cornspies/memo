//! SQLite access: one writer connection, a small pool of readers, embedded migrations.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use memo_core::b64_encode;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};

const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];
const IDLE_READERS: usize = 4;

pub struct Db {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
}

impl Db {
    /// Opens (creating if needed) and migrates the database.
    pub fn open(path: &Path) -> ApiResult<Self> {
        let mut writer = connect(path)?;
        migrate(&mut writer)?;
        Ok(Self { path: path.to_owned(), writer: Mutex::new(writer), readers: Mutex::new(Vec::new()) })
    }

    pub fn write_blocking<R>(&self, f: impl FnOnce(&Transaction) -> ApiResult<R>) -> ApiResult<R> {
        let mut conn = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn read_blocking<R>(&self, f: impl FnOnce(&Connection) -> ApiResult<R>) -> ApiResult<R> {
        let pooled = self.readers.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => connect(&self.path)?,
        };
        let result = f(&conn);
        let mut readers = self.readers.lock().unwrap_or_else(PoisonError::into_inner);
        if readers.len() < IDLE_READERS {
            readers.push(conn);
        }
        result
    }

    pub async fn write<R: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Transaction) -> ApiResult<R> + Send + 'static,
    ) -> ApiResult<R> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.write_blocking(f)).await.map_err(ApiError::internal)?
    }

    pub async fn read<R: Send + 'static>(
        self: &Arc<Self>,
        f: impl FnOnce(&Connection) -> ApiResult<R> + Send + 'static,
    ) -> ApiResult<R> {
        let db = self.clone();
        tokio::task::spawn_blocking(move || db.read_blocking(f)).await.map_err(ApiError::internal)?
    }
}

fn connect(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    Ok(conn)
}

fn migrate(conn: &mut Connection) -> ApiResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let has_meta: bool =
        tx.query_row("SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'meta')", [], |r| {
            r.get(0)
        })?;
    let current: usize = if has_meta {
        tx.query_row("SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'schema_version'", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0)
    } else {
        0
    };
    if current > MIGRATIONS.len() {
        return Err(ApiError::internal(format!(
            "database schema {current} is newer than this server ({}); upgrade memo-server",
            MIGRATIONS.len()
        )));
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        tx.execute_batch(sql)?;
        if index == 0 {
            set_epoch(&tx)?;
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
            [(index + 1).to_string()],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// A new random epoch makes every client run a reset sync.
pub fn set_epoch(conn: &Connection) -> rusqlite::Result<String> {
    let epoch = b64_encode(random_bytes::<16>());
    conn.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('epoch', ?1)", [&epoch])?;
    Ok(epoch)
}

pub fn epoch(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row("SELECT value FROM meta WHERE key = 'epoch'", [], |r| r.get(0))
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).expect("OS random number generator");
    bytes
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub trait RowExt {
    fn uuid(&self, idx: usize) -> rusqlite::Result<Uuid>;
    fn opt_uuid(&self, idx: usize) -> rusqlite::Result<Option<Uuid>>;
}

impl RowExt for Row<'_> {
    fn uuid(&self, idx: usize) -> rusqlite::Result<Uuid> {
        Ok(Uuid::from_bytes(self.get::<_, [u8; 16]>(idx)?))
    }

    fn opt_uuid(&self, idx: usize) -> rusqlite::Result<Option<Uuid>> {
        Ok(self.get::<_, Option<[u8; 16]>>(idx)?.map(Uuid::from_bytes))
    }
}

/// Entity types in `change_log`. The vault's `entity_id` is the user id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entity {
    Memo,
    Access,
    Attachment,
    State,
    Vault,
}

impl Entity {
    pub fn as_str(self) -> &'static str {
        match self {
            Entity::Memo => "memo",
            Entity::Access => "access",
            Entity::Attachment => "attachment",
            Entity::State => "state",
            Entity::Vault => "vault",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "memo" => Entity::Memo,
            "access" => Entity::Access,
            "attachment" => Entity::Attachment,
            "state" => Entity::State,
            "vault" => Entity::Vault,
            _ => return None,
        })
    }
}

/// Records a write for one user, replacing their earlier row for the entity.
pub fn log_change(tx: &Transaction, user_id: Uuid, entity: Entity, entity_id: Uuid) -> rusqlite::Result<()> {
    tx.execute(
        "REPLACE INTO change_log (user_id, entity_type, entity_id, changed_at) VALUES (?1, ?2, ?3, ?4)",
        params![user_id.as_bytes(), entity.as_str(), entity_id.as_bytes(), now_ms()],
    )?;
    Ok(())
}

/// Records a write for every member of a memo.
pub fn log_for_members(tx: &Transaction, memo_id: Uuid, entity: Entity, entity_id: Uuid) -> rusqlite::Result<()> {
    tx.execute(
        "REPLACE INTO change_log (user_id, entity_type, entity_id, changed_at)
         SELECT user_id, ?2, ?3, ?4 FROM memo_members WHERE memo_id = ?1",
        params![memo_id.as_bytes(), entity.as_str(), entity_id.as_bytes(), now_ms()],
    )?;
    Ok(())
}

pub fn members_of(conn: &Connection, memo_id: Uuid) -> rusqlite::Result<Vec<Uuid>> {
    let mut stmt = conn.prepare_cached("SELECT user_id FROM memo_members WHERE memo_id = ?1")?;
    stmt.query_map([memo_id.as_bytes()], |r| r.uuid(0))?.collect()
}

/// `None` if the user is not a member.
pub fn permission(conn: &Connection, memo_id: Uuid, user_id: Uuid) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT permission FROM memo_members WHERE memo_id = ?1 AND user_id = ?2",
        params![memo_id.as_bytes(), user_id.as_bytes()],
        |r| r.get(0),
    )
    .optional()
}

/// Bytes counted against a user's quota: their memos' revisions and the blobs they uploaded.
pub fn used_bytes(conn: &Connection, user_id: Uuid) -> rusqlite::Result<u64> {
    let used: i64 = conn.query_row(
        "SELECT
           COALESCE((SELECT SUM(LENGTH(r.ciphertext)) FROM memo_revisions r JOIN memos m ON m.id = r.memo_id
                     WHERE m.owner_id = ?1), 0)
         + COALESCE((SELECT SUM(COALESCE(size, 0) + COALESCE(thumb_size, 0)) FROM attachments
                     WHERE uploader_id = ?1 AND purged_at IS NULL), 0)",
        [user_id.as_bytes()],
        |r| r.get(0),
    )?;
    Ok(used as u64)
}

pub fn quota_bytes(conn: &Connection, user_id: Uuid, default: u64) -> rusqlite::Result<u64> {
    let quota: Option<i64> = conn
        .query_row("SELECT quota_bytes FROM users WHERE id = ?1", [user_id.as_bytes()], |r| r.get(0))
        .optional()?
        .flatten();
    Ok(quota.map_or(default, |q| q as u64))
}

/// Whether `extra` more bytes fit in the user's quota.
pub fn fits_quota(conn: &Connection, user_id: Uuid, extra: u64, default_quota: u64) -> rusqlite::Result<bool> {
    Ok(used_bytes(conn, user_id)?.saturating_add(extra) <= quota_bytes(conn, user_id, default_quota)?)
}
