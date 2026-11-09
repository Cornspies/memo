//! The [`Store`] of the native clients (Android, desktop): one SQLite file per account.
//!
//! Records are stored as JSON, next to the columns the engine looks them up by. Attachment blobs
//! (encrypted envelopes) live in the same file. Downloaded ones are a cache: [`SqliteStore::trim_blobs`]
//! drops the least recently used, but never one still waiting to be uploaded.

use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use memo_core::{
    AttachmentKind, Error, Result,
    sync::{AccessRecord, AttachmentRecord, MemoRecord, StateRecord, Store, SyncMeta, VaultRecord},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

const SCHEMA: &str = "
CREATE TABLE meta (one INTEGER PRIMARY KEY CHECK (one = 1), data TEXT NOT NULL);
CREATE TABLE memos (
  id        BLOB PRIMARY KEY,
  dirty     INTEGER NOT NULL,
  undecoded INTEGER NOT NULL,
  data      TEXT NOT NULL
);
CREATE INDEX memos_dirty ON memos (id) WHERE dirty;
CREATE INDEX memos_undecoded ON memos (id) WHERE undecoded;
CREATE TABLE access (memo_id BLOB PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE states (memo_id BLOB PRIMARY KEY, dirty INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE attachments (
  id        BLOB PRIMARY KEY,
  memo_id   BLOB NOT NULL,
  -- an upload is pending: its blobs are never evicted
  uploading INTEGER NOT NULL,
  pending   INTEGER NOT NULL,
  data      TEXT NOT NULL
);
CREATE INDEX attachments_memo ON attachments (memo_id);
CREATE INDEX attachments_pending ON attachments (id) WHERE pending;
CREATE TABLE vault (one INTEGER PRIMARY KEY CHECK (one = 1), data TEXT NOT NULL);
CREATE TABLE blobs (
  id      BLOB NOT NULL,
  kind    INTEGER NOT NULL,
  data    BLOB NOT NULL,
  used_at INTEGER NOT NULL,
  PRIMARY KEY (id, kind)
);
CREATE INDEX blobs_used ON blobs (used_at);
";
const SCHEMA_VERSION: i64 = 1;

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self> {
        Self::setup(Connection::open(path).map_err(store)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::setup(Connection::open_in_memory().map_err(store)?)
    }

    fn setup(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").map_err(store)?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(store)?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(store)?;
        match version {
            0 => {
                conn.execute_batch(&format!("BEGIN; {SCHEMA} PRAGMA user_version = {SCHEMA_VERSION}; COMMIT;"))
                    .map_err(store)?;
            }
            SCHEMA_VERSION => {}
            _ => return Err(Error::Store(format!("database schema {version} is newer than this app"))),
        }
        Ok(Self { conn })
    }

    /// Evicts cached blobs, least recently used first, until they take at most `budget` bytes.
    /// Blobs of attachments with a pending upload stay and do not count. Returns the bytes freed.
    pub fn trim_blobs(&mut self, budget: u64) -> Result<u64> {
        let tx = self.conn.transaction().map_err(store)?;
        let mut evict = Vec::new();
        {
            let mut stmt = tx
                .prepare(
                    "SELECT b.id, b.kind, length(b.data) FROM blobs b
                     WHERE NOT EXISTS (SELECT 1 FROM attachments a WHERE a.id = b.id AND a.uploading)
                     ORDER BY b.used_at DESC, b.rowid DESC",
                )
                .map_err(store)?;
            let mut rows = stmt.query([]).map_err(store)?;
            let mut kept = 0u64;
            while let Some(row) = rows.next().map_err(store)? {
                let size: u64 = row.get::<_, i64>(2).map_err(store)? as u64;
                if kept + size <= budget {
                    kept += size;
                } else {
                    evict.push((row.get::<_, Vec<u8>>(0).map_err(store)?, row.get::<_, i64>(1).map_err(store)?, size));
                }
            }
        }
        let mut freed = 0;
        for (id, kind, size) in evict {
            tx.execute("DELETE FROM blobs WHERE id = ?1 AND kind = ?2", params![id, kind]).map_err(store)?;
            freed += size;
        }
        tx.commit().map_err(store)?;
        Ok(freed)
    }

    fn get<T: DeserializeOwned>(&self, sql: &str, params: impl rusqlite::Params) -> Result<Option<T>> {
        let data: Option<String> =
            self.conn.prepare_cached(sql).map_err(store)?.query_row(params, |r| r.get(0)).optional().map_err(store)?;
        data.map(|d| from_json(&d)).transpose()
    }

    fn ids(&self, sql: &str) -> Result<Vec<Uuid>> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(store)?;
        let rows = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).map_err(store)?;
        rows.map(|r| r.map_err(store).and_then(|b| uuid(&b))).collect()
    }

    fn exec(&self, sql: &str, params: impl rusqlite::Params) -> Result<()> {
        self.conn.prepare_cached(sql).map_err(store)?.execute(params).map_err(store)?;
        Ok(())
    }
}

impl Store for SqliteStore {
    fn begin(&mut self) -> Result<()> {
        self.exec("BEGIN IMMEDIATE", [])
    }

    fn commit(&mut self) -> Result<()> {
        self.exec("COMMIT", [])
    }

    fn rollback(&mut self) {
        // Fails only if no transaction is open, and then there is nothing to undo.
        let _ = self.conn.execute_batch("ROLLBACK");
    }

    fn meta(&self) -> Result<SyncMeta> {
        Ok(self.get("SELECT data FROM meta", [])?.unwrap_or_default())
    }

    fn set_meta(&mut self, meta: &SyncMeta) -> Result<()> {
        self.exec("INSERT OR REPLACE INTO meta (one, data) VALUES (1, ?1)", [json(meta)])
    }

    fn memo(&self, id: Uuid) -> Result<Option<MemoRecord>> {
        self.get("SELECT data FROM memos WHERE id = ?1", [&id.as_bytes()[..]])
    }

    fn memo_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT id FROM memos ORDER BY id")
    }

    fn put_memo(&mut self, memo: &MemoRecord) -> Result<()> {
        self.exec(
            "INSERT OR REPLACE INTO memos (id, dirty, undecoded, data) VALUES (?1, ?2, ?3, ?4)",
            params![&memo.id.as_bytes()[..], memo.is_dirty(), memo.undecoded.is_some(), json(memo)],
        )
    }

    fn delete_memo(&mut self, id: Uuid) -> Result<()> {
        self.exec("DELETE FROM memos WHERE id = ?1", [&id.as_bytes()[..]])
    }

    fn access(&self, memo_id: Uuid) -> Result<Option<AccessRecord>> {
        self.get("SELECT data FROM access WHERE memo_id = ?1", [&memo_id.as_bytes()[..]])
    }

    fn put_access(&mut self, access: &AccessRecord) -> Result<()> {
        self.exec(
            "INSERT OR REPLACE INTO access (memo_id, data) VALUES (?1, ?2)",
            params![&access.memo_id.as_bytes()[..], json(access)],
        )
    }

    fn delete_access(&mut self, memo_id: Uuid) -> Result<()> {
        self.exec("DELETE FROM access WHERE memo_id = ?1", [&memo_id.as_bytes()[..]])
    }

    fn state(&self, memo_id: Uuid) -> Result<Option<StateRecord>> {
        self.get("SELECT data FROM states WHERE memo_id = ?1", [&memo_id.as_bytes()[..]])
    }

    fn state_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT memo_id FROM states ORDER BY memo_id")
    }

    fn put_state(&mut self, state: &StateRecord) -> Result<()> {
        self.exec(
            "INSERT OR REPLACE INTO states (memo_id, dirty, data) VALUES (?1, ?2, ?3)",
            params![&state.memo_id.as_bytes()[..], state.dirty, json(state)],
        )
    }

    fn delete_state(&mut self, memo_id: Uuid) -> Result<()> {
        self.exec("DELETE FROM states WHERE memo_id = ?1", [&memo_id.as_bytes()[..]])
    }

    fn attachment(&self, id: Uuid) -> Result<Option<AttachmentRecord>> {
        self.get("SELECT data FROM attachments WHERE id = ?1", [&id.as_bytes()[..]])
    }

    fn attachment_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT id FROM attachments ORDER BY id")
    }

    fn put_attachment(&mut self, a: &AttachmentRecord) -> Result<()> {
        let uploading = a.upload_original || a.upload_thumbnail;
        self.exec(
            "INSERT OR REPLACE INTO attachments (id, memo_id, uploading, pending, data) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&a.id.as_bytes()[..], &a.memo_id.as_bytes()[..], uploading, uploading || a.pending_delete, json(a)],
        )
    }

    fn delete_attachment(&mut self, id: Uuid) -> Result<()> {
        self.exec("DELETE FROM attachments WHERE id = ?1", [&id.as_bytes()[..]])?;
        self.exec("DELETE FROM blobs WHERE id = ?1", [&id.as_bytes()[..]])
    }

    fn vault(&self) -> Result<Option<VaultRecord>> {
        self.get("SELECT data FROM vault", [])
    }

    fn put_vault(&mut self, vault: &VaultRecord) -> Result<()> {
        self.exec("INSERT OR REPLACE INTO vault (one, data) VALUES (1, ?1)", [json(vault)])
    }

    fn blob(&self, id: Uuid, kind: AttachmentKind) -> Result<Option<Vec<u8>>> {
        let key = params![&id.as_bytes()[..], kind_code(kind)];
        let data: Option<Vec<u8>> = self
            .conn
            .prepare_cached("SELECT data FROM blobs WHERE id = ?1 AND kind = ?2")
            .map_err(store)?
            .query_row(key, |r| r.get(0))
            .optional()
            .map_err(store)?;
        if data.is_some() {
            self.exec(
                "UPDATE blobs SET used_at = ?3 WHERE id = ?1 AND kind = ?2",
                params![&id.as_bytes()[..], kind_code(kind), now()],
            )?;
        }
        Ok(data)
    }

    fn put_blob(&mut self, id: Uuid, kind: AttachmentKind, envelope: &[u8]) -> Result<()> {
        self.exec(
            "INSERT OR REPLACE INTO blobs (id, kind, data, used_at) VALUES (?1, ?2, ?3, ?4)",
            params![&id.as_bytes()[..], kind_code(kind), envelope, now()],
        )
    }

    fn dirty_memo_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT id FROM memos WHERE dirty ORDER BY id")
    }

    fn undecoded_memo_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT id FROM memos WHERE undecoded ORDER BY id")
    }

    fn dirty_state_ids(&self) -> Result<Vec<Uuid>> {
        self.ids("SELECT memo_id FROM states WHERE dirty ORDER BY memo_id")
    }

    fn attachments_of(&self, memo_id: Uuid) -> Result<Vec<AttachmentRecord>> {
        self.records("SELECT data FROM attachments WHERE memo_id = ?1 ORDER BY id", [&memo_id.as_bytes()[..]])
    }

    fn pending_attachments(&self) -> Result<Vec<AttachmentRecord>> {
        self.records("SELECT data FROM attachments WHERE pending ORDER BY id", [])
    }
}

impl SqliteStore {
    fn records<T: DeserializeOwned>(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<T>> {
        let mut stmt = self.conn.prepare_cached(sql).map_err(store)?;
        let rows = stmt.query_map(params, |r| r.get::<_, String>(0)).map_err(store)?;
        rows.map(|r| r.map_err(store).and_then(|d| from_json(&d))).collect()
    }
}

fn store(e: rusqlite::Error) -> Error {
    Error::Store(e.to_string())
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("records serialize")
}

fn from_json<T: DeserializeOwned>(data: &str) -> Result<T> {
    serde_json::from_str(data).map_err(|e| Error::Store(format!("corrupt record: {e}")))
}

fn uuid(bytes: &[u8]) -> Result<Uuid> {
    Uuid::from_slice(bytes).map_err(|_| Error::Store("corrupt id".into()))
}

fn kind_code(kind: AttachmentKind) -> i64 {
    match kind {
        AttachmentKind::Original => 0,
        AttachmentKind::Thumbnail => 1,
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests;
