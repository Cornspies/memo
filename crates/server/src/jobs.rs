//! Garbage collection, hourly and via `memo-server gc` (`docs/server.md#background-jobs`).

use std::{
    collections::HashSet,
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

use memo_core::AttachmentKind;
use rusqlite::params;
use uuid::Uuid;

use crate::{
    config::Config,
    db::{Db, RowExt, now_ms},
    error::ApiResult,
    state::blob_path,
};

const DAY_MS: i64 = 24 * 3600 * 1000;
const ORPHAN_AGE: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Default, PartialEq, Eq)]
pub struct GcReport {
    pub blobs_purged: usize,
    pub orphans_removed: usize,
    pub log_rows_purged: usize,
    pub revisions_removed: usize,
}

pub fn gc(db: &Db, config: &Config) -> ApiResult<GcReport> {
    gc_at(db, config, now_ms())
}

pub fn gc_at(db: &Db, config: &Config, now: i64) -> ApiResult<GcReport> {
    let mut report = GcReport::default();
    let blob_cutoff = now - i64::from(config.retention.deleted_blob_days) * DAY_MS;
    let tombstone_cutoff = now - i64::from(config.retention.tombstone_days) * DAY_MS;

    // Blobs of deleted attachments: mark first, then remove the files. A crash in between only
    // leaves orphans, which the next run cleans up.
    let purged: Vec<Uuid> = db.write_blocking(|tx| {
        let ids: Vec<Uuid> = tx
            .prepare("SELECT id FROM attachments WHERE deleted_at < ?1 AND purged_at IS NULL")?
            .query_map([blob_cutoff], |r| r.uuid(0))?
            .collect::<Result<_, _>>()?;
        tx.execute(
            "UPDATE attachments SET purged_at = ?1 WHERE deleted_at < ?2 AND purged_at IS NULL",
            params![now, blob_cutoff],
        )?;
        Ok(ids)
    })?;
    for id in &purged {
        for kind in [AttachmentKind::Original, AttachmentKind::Thumbnail] {
            remove_if_exists(&blob_path(&config.data_dir, *id, kind))?;
        }
    }
    report.blobs_purged = purged.len();

    report.log_rows_purged = db.write_blocking(|tx| {
        // Tombstones: deleted memos and attachments, and access/state rows of former members.
        tx.execute(
            "CREATE TEMP TABLE purge AS SELECT seq, user_id FROM change_log c WHERE
               (c.entity_type = 'memo' AND NOT EXISTS (SELECT 1 FROM memos m WHERE m.id = c.entity_id
                                                       AND (m.deleted = 0 OR m.deleted_at >= ?1)))
            OR (c.entity_type = 'attachment' AND NOT EXISTS (SELECT 1 FROM attachments a WHERE a.id = c.entity_id
                                                             AND (a.deleted_at IS NULL OR a.deleted_at >= ?1)))
            OR (c.entity_type IN ('access', 'state') AND c.changed_at < ?1
                AND NOT EXISTS (SELECT 1 FROM memo_members mm WHERE mm.memo_id = c.entity_id AND mm.user_id = c.user_id))",
            [tombstone_cutoff],
        )?;
        tx.execute(
            "UPDATE users SET purged_through = MAX(purged_through, (SELECT MAX(seq) FROM purge p WHERE p.user_id = users.id))
             WHERE id IN (SELECT user_id FROM purge)",
            [],
        )?;
        let purged = tx.execute("DELETE FROM change_log WHERE seq IN (SELECT seq FROM purge)", [])?;
        tx.execute("DROP TABLE temp.purge", [])?;
        // Tombstoned memos nobody references any more go, with their attachment rows.
        tx.execute(
            "DELETE FROM memos WHERE deleted = 1 AND deleted_at < ?1
               AND NOT EXISTS (SELECT 1 FROM change_log c WHERE c.entity_type = 'memo' AND c.entity_id = memos.id)",
            [tombstone_cutoff],
        )?;
        tx.execute(
            "DELETE FROM attachments WHERE purged_at IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM change_log c WHERE c.entity_type = 'attachment' AND c.entity_id = attachments.id)",
            [],
        )?;
        tx.execute("DELETE FROM recovery_tokens WHERE expires_at < ?1", [now])?;
        tx.execute("DELETE FROM invites WHERE used_at IS NULL AND expires_at < ?1", [now])?;
        // Every sign-out and every web unlock leaves a revoked device behind.
        tx.execute("DELETE FROM devices WHERE revoked_at < ?1", [tombstone_cutoff])?;
        Ok(purged)
    })?;

    let keep = config.retention.revisions_per_memo;
    if keep > 0 {
        report.revisions_removed = db.write_blocking(|tx| {
            Ok(tx.execute(
                "DELETE FROM memo_revisions WHERE version <= (SELECT m.version FROM memos m WHERE m.id = memo_revisions.memo_id) - ?1",
                [keep],
            )?)
        })?;
    }

    report.orphans_removed = remove_orphans(db, config)?;
    Ok(report)
}

/// Blob files without a live attachment row, and stale temporary files, older than a day.
fn remove_orphans(db: &Db, config: &Config) -> ApiResult<usize> {
    let mut removed = 0;
    let old = |path: &Path| -> bool {
        fs::metadata(path)
            .and_then(|m| m.modified())
            .is_ok_and(|t| SystemTime::now().duration_since(t).unwrap_or_default() > ORPHAN_AGE)
    };
    for entry in read_dir(&config.data_dir.join("tmp"))? {
        if old(&entry) {
            remove_if_exists(&entry)?;
            removed += 1;
        }
    }
    // A blob stored after this was read is not in it, but it is not a day old either.
    let live: HashSet<Uuid> = db.read_blocking(|conn| {
        Ok(conn
            .prepare("SELECT id FROM attachments WHERE purged_at IS NULL")?
            .query_map([], |r| r.uuid(0))?
            .collect::<Result<_, _>>()?)
    })?;
    for dir in read_dir(&config.data_dir.join("blobs"))? {
        for file in read_dir(&dir)? {
            let id = file.file_stem().and_then(|s| s.to_str()).and_then(|s| Uuid::try_parse(s).ok());
            let live = id.is_some_and(|id| live.contains(&id));
            if !live && old(&file) {
                remove_if_exists(&file)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

fn read_dir(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries.map(|e| e.map(|e| e.path())).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}
