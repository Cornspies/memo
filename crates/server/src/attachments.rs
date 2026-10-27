//! Attachment blobs (`docs/api.md#attachments`). Uploads stream to `tmp/` while hashing, are
//! fsynced and renamed into `blobs/`, and only then is the database row committed.

use std::path::{Path as FsPath, PathBuf};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use memo_core::{AttachmentKind, ENVELOPE_OVERHEAD};
use rusqlite::{OptionalExtension, params};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    auth::Session,
    db::{self, Entity, RowExt, now_ms, random_bytes},
    error::{ApiError, ApiResult},
    memos::require_write,
    state::App,
    sync::visible_memo,
};

fn parse_kind(kind: &str) -> ApiResult<AttachmentKind> {
    match kind {
        "original" => Ok(AttachmentKind::Original),
        "thumbnail" => Ok(AttachmentKind::Thumbnail),
        _ => Err(ApiError::not_found()),
    }
}

/// Column names for one kind.
fn columns(kind: AttachmentKind) -> (&'static str, &'static str) {
    match kind {
        AttachmentKind::Original => ("size", "sha256"),
        AttachmentKind::Thumbnail => ("thumb_size", "thumb_sha256"),
    }
}

/// Deletes the temporary file unless it was moved into place.
struct TempFile(Option<PathBuf>);

impl TempFile {
    fn keep(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub async fn upload(
    State(app): State<App>,
    session: Session,
    Path((memo_id, id, kind)): Path<(Uuid, Uuid, String)>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<StatusCode> {
    let kind = parse_kind(&kind)?;
    let max = match kind {
        AttachmentKind::Original => app.config.limits.max_attachment.0,
        AttachmentKind::Thumbnail => app.config.limits.max_thumbnail.0,
    };
    let user = session.user_id;
    app.db
        .read(move |conn| {
            let (.., permission) = visible_memo(conn, memo_id, user)?;
            require_write(permission)?;
            check_attachment_memo(conn, id, memo_id)
        })
        .await?;
    let declared =
        headers.get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > max) {
        return Err(ApiError::too_large());
    }

    let tmp_path = app.tmp_dir().join(format!("{}.part", memo_core::b64_encode(random_bytes::<12>())));
    let mut tmp = TempFile(Some(tmp_path.clone()));
    let mut file = tokio::fs::File::create(&tmp_path).await?;
    let (mut hasher, mut size) = (Sha256::new(), 0u64);
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| ApiError::bad_request(format!("reading body: {e}")))?;
        let Ok(data) = frame.into_data() else { continue };
        size += data.len() as u64;
        if size > max {
            return Err(ApiError::too_large());
        }
        hasher.update(&data);
        file.write_all(&data).await?;
    }
    file.sync_all().await?;
    drop(file);
    if size <= ENVELOPE_OVERHEAD as u64 {
        return Err(ApiError::bad_request("not an encrypted attachment"));
    }
    let digest: [u8; 32] = hasher.finalize().into();
    let blob_path = app.blob_path(id, kind);
    let default_quota = app.config.limits.default_quota.0;

    app.db
        .write(move |tx| {
            // Checked again: things may have changed while the body streamed in.
            let (.., permission) = visible_memo(tx, memo_id, user)?;
            require_write(permission)?;
            check_attachment_memo(tx, id, memo_id)?;
            let (size_col, sha_col) = columns(kind);
            let stored: Option<Option<[u8; 32]>> = tx
                .query_row(&format!("SELECT {sha_col} FROM attachments WHERE id = ?1"), [id.as_bytes()], |r| r.get(0))
                .optional()?;
            match stored.flatten() {
                Some(existing) if existing == digest => return Ok(StatusCode::OK),
                Some(_) => return Err(ApiError::conflict("already stored with different content")),
                None => {}
            }
            if !db::fits_quota(tx, user, size, default_quota)? {
                return Err(ApiError::quota_exceeded());
            }
            move_into_place(&tmp_path, &blob_path)?;
            tmp.keep();
            tx.execute(
                "INSERT INTO attachments (id, memo_id, uploader_id, created_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (id) DO NOTHING",
                params![id.as_bytes(), memo_id.as_bytes(), user.as_bytes(), now_ms()],
            )?;
            tx.execute(&format!("UPDATE attachments SET {size_col} = ?1, {sha_col} = ?2 WHERE id = ?3"), params![size, digest, id.as_bytes()])?;
            db::log_for_members(tx, memo_id, Entity::Attachment, id)?;
            Ok(StatusCode::CREATED)
        })
        .await
}

/// An existing attachment id must belong to this memo and not be deleted.
fn check_attachment_memo(conn: &rusqlite::Connection, id: Uuid, memo_id: Uuid) -> ApiResult<()> {
    let row = conn
        .query_row("SELECT memo_id, deleted_at IS NOT NULL FROM attachments WHERE id = ?1", [id.as_bytes()], |r| {
            Ok((r.uuid(0)?, r.get::<_, bool>(1)?))
        })
        .optional()?;
    match row {
        Some((owner_memo, _)) if owner_memo != memo_id => Err(ApiError::conflict("attachment belongs to another memo")),
        Some((_, true)) => Err(ApiError::not_found()),
        _ => Ok(()),
    }
}

fn move_into_place(tmp: &FsPath, target: &FsPath) -> std::io::Result<()> {
    let dir = target.parent().expect("blob paths have a parent");
    std::fs::create_dir_all(dir)?;
    std::fs::rename(tmp, target)?;
    std::fs::File::open(dir)?.sync_all()
}

pub async fn download(
    State(app): State<App>,
    session: Session,
    Path((id, kind)): Path<(Uuid, String)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let kind = parse_kind(&kind)?;
    let user = session.user_id;
    let digest = app
        .db
        .read(move |conn| {
            let (_, sha_col) = columns(kind);
            let row = conn
                .query_row(
                    &format!("SELECT memo_id, {sha_col}, deleted_at IS NOT NULL FROM attachments WHERE id = ?1"),
                    [id.as_bytes()],
                    |r| Ok((r.uuid(0)?, r.get::<_, Option<[u8; 32]>>(1)?, r.get::<_, bool>(2)?)),
                )
                .optional()?;
            let (memo_id, digest, deleted) = row.ok_or_else(ApiError::not_found)?;
            visible_memo(conn, memo_id, user)?;
            match digest {
                Some(digest) if !deleted => Ok(digest),
                _ => Err(ApiError::not_found()),
            }
        })
        .await?;
    let etag = format!("\"{}\"", digest.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let etag = HeaderValue::from_str(&etag).expect("hex is ASCII");
    let mut response = if headers.get(header::IF_NONE_MATCH) == Some(&etag) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        let file = tokio::fs::File::open(app.blob_path(id, kind))
            .await
            .map_err(|e| ApiError::internal(format!("blob {id}: {e}")))?;
        let length = file.metadata().await?.len();
        let mut response = Body::from_stream(tokio_util::io::ReaderStream::new(file)).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment"));
        response
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("private, max-age=31536000, immutable"));
    headers.insert(header::ETAG, etag);
    Ok(response)
}
