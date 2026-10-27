//! Admin CLI commands (`docs/server.md#admin-cli`). They work on the database directly, so they
//! run on the server host; `backup` is safe while the server runs, `restore` is not.

use std::{
    fs,
    path::{Path, PathBuf},
};

use memo_core::b64_encode;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::{
    account::delete_user,
    auth::normalize_username,
    db::{self, Db, RowExt, now_ms, random_bytes, sha256},
    error::{ApiError, ApiResult},
    state::db_path,
};

pub struct Invite {
    /// First 8 hex characters of the code's hash: how `invite list` and `invite revoke` name it.
    pub id: String,
    pub role: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_by: Option<String>,
}

/// Returns the code. Only its hash is stored, so it is shown once.
pub fn invite_create(db: &Db, role: &str, valid_ms: i64) -> ApiResult<String> {
    if !matches!(role, "user" | "admin") {
        return Err(ApiError::bad_request("role must be user or admin"));
    }
    let code = b64_encode(random_bytes::<16>());
    let now = now_ms();
    db.write_blocking(|tx| {
        tx.execute(
            "INSERT INTO invites (code_hash, role, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)",
            params![sha256(code.as_bytes()), role, now, now + valid_ms],
        )?;
        Ok(())
    })?;
    Ok(code)
}

pub fn invite_list(db: &Db) -> ApiResult<Vec<Invite>> {
    db.read_blocking(|conn| {
        Ok(conn
            .prepare(
                "SELECT lower(hex(i.code_hash)), i.role, i.created_at, i.expires_at, u.username
                 FROM invites i LEFT JOIN users u ON u.id = i.used_by ORDER BY i.created_at",
            )?
            .query_map([], |r| {
                Ok(Invite {
                    id: r.get::<_, String>(0)?[..8].to_owned(),
                    role: r.get(1)?,
                    created_at: r.get(2)?,
                    expires_at: r.get(3)?,
                    used_by: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?)
    })
}

pub fn invite_revoke(db: &Db, id_prefix: &str) -> ApiResult<()> {
    let prefix = id_prefix.to_lowercase();
    if prefix.len() < 4 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ApiError::bad_request("give at least 4 hex characters of the id from `invite list`"));
    }
    db.write_blocking(|tx| {
        let matching: Vec<Vec<u8>> = tx
            .prepare("SELECT code_hash FROM invites WHERE lower(hex(code_hash)) LIKE ?1 || '%'")?
            .query_map([&prefix], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        match matching.as_slice() {
            [hash] => {
                tx.execute("DELETE FROM invites WHERE code_hash = ?1", [hash])?;
                Ok(())
            }
            [] => Err(ApiError::not_found()),
            _ => Err(ApiError::bad_request("ambiguous id; give more characters")),
        }
    })
}

pub struct UserRow {
    pub username: String,
    pub role: String,
    pub disabled: bool,
    pub used_bytes: u64,
    pub quota_bytes: u64,
    pub created_at: i64,
}

pub fn user_list(db: &Db, default_quota: u64) -> ApiResult<Vec<UserRow>> {
    db.read_blocking(|conn| {
        let users: Vec<(Uuid, String, String, bool, i64)> = conn
            .prepare("SELECT id, username, role, disabled, created_at FROM users ORDER BY username")?
            .query_map([], |r| Ok((r.uuid(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<Result<_, _>>()?;
        users
            .into_iter()
            .map(|(id, username, role, disabled, created_at)| {
                Ok(UserRow {
                    username,
                    role,
                    disabled,
                    used_bytes: db::used_bytes(conn, id)?,
                    quota_bytes: db::quota_bytes(conn, id, default_quota)?,
                    created_at,
                })
            })
            .collect()
    })
}

fn user_id(tx: &rusqlite::Connection, username: &str) -> ApiResult<Uuid> {
    let username = normalize_username(username)?;
    tx.query_row("SELECT id FROM users WHERE username = ?1", [&username], |r| r.uuid(0))
        .optional()?
        .ok_or_else(ApiError::not_found)
}

/// Disabling blocks login and revokes every device; shared memos stay readable for members.
pub fn user_set_disabled(db: &Db, username: &str, disabled: bool) -> ApiResult<()> {
    db.write_blocking(|tx| {
        let id = user_id(tx, username)?;
        tx.execute("UPDATE users SET disabled = ?1 WHERE id = ?2", params![disabled, id.as_bytes()])?;
        if disabled {
            tx.execute(
                "UPDATE devices SET revoked_at = ?1 WHERE user_id = ?2 AND revoked_at IS NULL",
                params![now_ms(), id.as_bytes()],
            )?;
        }
        Ok(())
    })
}

/// `None` resets to the server default.
pub fn user_set_quota(db: &Db, username: &str, quota: Option<u64>) -> ApiResult<()> {
    db.write_blocking(|tx| {
        let id = user_id(tx, username)?;
        tx.execute("UPDATE users SET quota_bytes = ?1 WHERE id = ?2", params![quota, id.as_bytes()])?;
        Ok(())
    })
}

pub fn user_delete(db: &Db, username: &str) -> ApiResult<()> {
    db.write_blocking(|tx| {
        let id = user_id(tx, username)?;
        delete_user(tx, id)
    })
}

/// A consistent snapshot of the database (`VACUUM INTO`), the secret, and the blobs (hard
/// links where possible). Safe while the server runs.
pub fn backup(db: &Db, data_dir: &Path, target: &Path) -> ApiResult<()> {
    if target.exists() && fs::read_dir(target)?.next().is_some() {
        return Err(ApiError::bad_request(format!("{} is not empty", target.display())));
    }
    fs::create_dir_all(target)?;
    let snapshot = db_path(target);
    db.read_blocking(|conn| {
        conn.execute(
            "VACUUM INTO ?1",
            [snapshot.to_str().ok_or_else(|| ApiError::bad_request("backup path is not UTF-8"))?],
        )?;
        Ok(())
    })?;
    fs::copy(data_dir.join("secret"), target.join("secret"))?;
    link_tree(&data_dir.join("blobs"), &target.join("blobs"))?;
    Ok(())
}

/// Puts a backup in place and generates a new epoch, so every client resyncs and pushes back
/// what the backup lacks. Stop the server first.
pub fn restore(source: &Path, data_dir: &Path) -> ApiResult<()> {
    let snapshot = db_path(source);
    if !snapshot.exists() || !source.join("secret").exists() {
        return Err(ApiError::bad_request(format!("{} does not look like a memo backup", source.display())));
    }
    fs::create_dir_all(data_dir)?;
    let live = db_path(data_dir);
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", live.display()));
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::copy(&snapshot, &live)?;
    fs::copy(source.join("secret"), data_dir.join("secret"))?;
    let blobs = data_dir.join("blobs");
    if blobs.exists() {
        fs::remove_dir_all(&blobs)?;
    }
    link_tree(&source.join("blobs"), &blobs)?;
    let db = Db::open(&live)?;
    db.write_blocking(|tx| Ok(db::set_epoch(tx)?))?;
    Ok(())
}

/// Copies a directory tree, hard-linking files when both sides are on the same filesystem.
fn link_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    if !from.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            link_tree(&entry.path(), &target)?;
        } else if fs::hard_link(entry.path(), &target).is_err() {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// `2026-10-26 14:03 UTC`, without pulling in a date library.
pub fn format_time(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rest) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", rest / 3600, rest % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_times() {
        assert_eq!(format_time(0), "1970-01-01 00:00 UTC");
        assert_eq!(format_time(1_761_480_000_000), "2025-10-26 12:00 UTC");
        assert_eq!(format_time(951_782_400_000), "2000-02-29 00:00 UTC");
    }
}
