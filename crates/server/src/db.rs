//! SQLite access: one writer connection, a small pool of readers, embedded migrations.

use std::{
    collections::{HashMap, hash_map::Entry},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use memo_core::b64_encode;
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    error::{ApiError, ApiResult},
    state::create_private_file,
};

const MIGRATIONS: &[&str] = &[include_str!("../migrations/0001_init.sql")];
/// Reader connections open at a time. Further reads wait for one.
const MAX_READERS: usize = 8;
/// Prepared statements each connection keeps (`prepare_cached`).
const STATEMENT_CACHE: usize = 64;

pub struct Db {
    path: PathBuf,
    writer: Mutex<Connection>,
    readers: Mutex<Readers>,
    reader_freed: Condvar,
}

#[derive(Default)]
struct Readers {
    idle: Vec<Connection>,
    open: usize,
}

/// A reader connection on loan. Goes back to the pool when dropped, also if the read panics.
struct Reader<'a> {
    db: &'a Db,
    conn: Option<Connection>,
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        let mut readers = self.db.readers.lock().unwrap_or_else(PoisonError::into_inner);
        match self.conn.take() {
            Some(conn) => readers.idle.push(conn),
            None => readers.open -= 1,
        }
        self.db.reader_freed.notify_one();
    }
}

impl Db {
    /// Opens (creating if needed) and migrates the database.
    pub fn open(path: &Path) -> ApiResult<Self> {
        create_private_file(path)?;
        let mut writer = connect(path)?;
        migrate(&mut writer)?;
        Ok(Self {
            path: path.to_owned(),
            writer: Mutex::new(writer),
            readers: Mutex::default(),
            reader_freed: Condvar::new(),
        })
    }

    pub fn write_blocking<R>(&self, f: impl FnOnce(&Transaction) -> ApiResult<R>) -> ApiResult<R> {
        let mut conn = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = f(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn read_blocking<R>(&self, f: impl FnOnce(&Connection) -> ApiResult<R>) -> ApiResult<R> {
        let mut reader = self.reader();
        if reader.conn.is_none() {
            reader.conn = Some(connect(&self.path)?);
        }
        f(reader.conn.as_ref().expect("just opened"))
    }

    /// An idle reader, or the right to open one (`conn` is `None`). Waits if all are in use.
    fn reader(&self) -> Reader<'_> {
        let mut readers = self.readers.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(conn) = readers.idle.pop() {
                return Reader { db: self, conn: Some(conn) };
            }
            if readers.open < MAX_READERS {
                readers.open += 1;
                return Reader { db: self, conn: None };
            }
            readers = self.reader_freed.wait(readers).unwrap_or_else(PoisonError::into_inner);
        }
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
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
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
    tx.prepare_cached("REPLACE INTO change_log (user_id, entity_type, entity_id, changed_at) VALUES (?1, ?2, ?3, ?4)")?
        .execute(params![user_id.as_bytes(), entity.as_str(), entity_id.as_bytes(), now_ms()])?;
    Ok(())
}

/// Records a write for every member of a memo.
pub fn log_for_members(tx: &Transaction, memo_id: Uuid, entity: Entity, entity_id: Uuid) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "REPLACE INTO change_log (user_id, entity_type, entity_id, changed_at)
         SELECT user_id, ?2, ?3, ?4 FROM memo_members WHERE memo_id = ?1",
    )?
    .execute(params![memo_id.as_bytes(), entity.as_str(), entity_id.as_bytes(), now_ms()])?;
    Ok(())
}

/// A user lost access to a memo (revoked, or left). Only the `access` row stays in their log,
/// which renders as revoked: sync has nothing of the memo left to send them.
pub fn forget_memo(tx: &Transaction, memo_id: Uuid, user_id: Uuid) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM change_log WHERE user_id = ?1 AND (
           (entity_type IN ('memo', 'state') AND entity_id = ?2)
           OR (entity_type = 'attachment' AND entity_id IN (SELECT id FROM attachments WHERE memo_id = ?2)))",
        params![user_id.as_bytes(), memo_id.as_bytes()],
    )?;
    Ok(())
}

pub fn members_of(conn: &Connection, memo_id: Uuid) -> rusqlite::Result<Vec<Uuid>> {
    let mut stmt = conn.prepare_cached("SELECT user_id FROM memo_members WHERE memo_id = ?1")?;
    stmt.query_map([memo_id.as_bytes()], |r| r.uuid(0))?.collect()
}

/// `None` if the user is not a member.
pub fn permission(conn: &Connection, memo_id: Uuid, user_id: Uuid) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached("SELECT permission FROM memo_members WHERE memo_id = ?1 AND user_id = ?2")?
        .query_row(params![memo_id.as_bytes(), user_id.as_bytes()], |r| r.get(0))
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

/// The quota checks of one request. Adding up what a user stores reads all their revisions, so
/// it is done once per user and then kept up to date as the request stores more.
#[derive(Default)]
pub struct Quota {
    /// Bytes used and allowed.
    users: HashMap<Uuid, (u64, u64)>,
}

impl Quota {
    /// Whether `extra` more bytes fit. If so, they count as used from here on.
    pub fn take(&mut self, conn: &Connection, user_id: Uuid, extra: u64, default_quota: u64) -> rusqlite::Result<bool> {
        let (used, allowed) = match self.users.entry(user_id) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                entry.insert((used_bytes(conn, user_id)?, quota_bytes(conn, user_id, default_quota)?))
            }
        };
        if used.saturating_add(extra) > *allowed {
            return Ok(false);
        }
        *used += extra;
        Ok(true)
    }

    /// The user's bytes went down (a delete): add them up again when next asked.
    pub fn forget(&mut self, user_id: Uuid) {
        self.users.remove(&user_id);
    }
}
