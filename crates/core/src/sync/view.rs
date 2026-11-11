//! Read models for the UI: the memo list, one memo, tags and contact trust. They never expose keys.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::store::{MemoRecord, PushError, Store, UndecodedReason};
use crate::{
    Error, Result,
    api::{Permission, Revision, UserInfo},
    content::{MemoContext, open_memo},
    keys::PublicKey,
    payload::{MemoPayload, ThumbRef},
    share::fingerprint,
    text,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct MemoFilter {
    /// Words that must all appear in the body, ignoring case.
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub tag: Option<String>,
    /// The archive instead of the main list.
    #[serde(default)]
    pub archived: bool,
    /// What the memo must all contain.
    #[serde(default)]
    pub has: Vec<Has>,
    /// Created at or after this time (Unix ms). Clients pass the bounds of a local day.
    #[serde(default)]
    pub created_from: Option<i64>,
    /// Created before this time (Unix ms).
    #[serde(default)]
    pub created_before: Option<i64>,
}

/// The quick filters (`docs/crypto.md#vault-payload`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Has {
    Tasks,
    OpenTasks,
    Links,
    Images,
    Code,
}

impl Has {
    fn matches(self, contents: &text::Contents, images: usize) -> bool {
        match self {
            Has::Tasks => contents.open_tasks + contents.done_tasks > 0,
            Has::OpenTasks => contents.open_tasks > 0,
            Has::Links => contents.links,
            Has::Images => images > 0,
            Has::Code => contents.code,
        }
    }
}

/// A saved filter from the vault: a name for a search, a tag and quick filters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedFilter {
    #[serde(skip_deserializing)]
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub has: Vec<Has>,
}

pub(crate) const FILTER_PREFIX: &str = "filter/";

/// Counts over the memo list (the archive only as `archived`). Clients count the days with memos
/// from [`activity`], in their time zone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    pub memos: usize,
    pub archived: usize,
    pub words: usize,
    pub tags: usize,
    pub images: usize,
    pub open_tasks: usize,
    pub done_tasks: usize,
    /// Shared with someone, by the user or with the user.
    pub shared: usize,
    /// Creation time of the oldest memo.
    pub since: Option<i64>,
}

/// Another memo, as a link to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoLink {
    pub id: Uuid,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoSummary {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub title: String,
    pub preview: String,
    pub tags: Vec<String>,
    pub pinned: bool,
    pub archived: bool,
    /// Has members besides the owner.
    pub shared: bool,
    pub writable: bool,
    pub created_at: i64,
    /// The later of the server's last write and the local edit.
    pub updated_at: i64,
    /// Local changes not on the server yet.
    pub dirty: bool,
    pub conflict: bool,
    pub undecoded: Option<UndecodedReason>,
    pub push_error: Option<PushError>,
    pub images: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemoDetail {
    #[serde(flatten)]
    pub summary: MemoSummary,
    pub body: String,
    pub edited_at: i64,
    pub server_version: u64,
    pub updated_by: Option<Uuid>,
    pub own: bool,
    pub members: Vec<MemberView>,
    /// The body's images, and during a conflict also those of the other version.
    pub attachments: Vec<AttachmentView>,
    pub conflict_with: Option<ConflictView>,
    /// Memos that link to this one, by title.
    pub linked_from: Vec<MemoLink>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MemberView {
    pub user_id: Uuid,
    pub username: String,
    pub permission: Permission,
    pub trust: Trust,
    /// Of the pinned key, or of the presented one if there is no pin.
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    Me,
    /// Pinned, and the user compared fingerprints.
    Verified,
    Pinned,
    NotPinned,
    /// The server presents a different key than the pinned one. Possible attack.
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AttachmentView {
    pub id: Uuid,
    pub mime: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    pub thumb: Option<ThumbRef>,
    /// The original can be downloaded or is cached.
    pub available: bool,
    pub thumb_available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConflictView {
    pub body: String,
    pub version: u64,
    pub edited_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: usize,
}

/// The memo list: pinned first, then the most recently changed.
pub fn memo_list<S: Store>(store: &S, filter: &MemoFilter) -> Result<Vec<MemoSummary>> {
    let terms: Vec<String> = filter.query.split_whitespace().map(str::to_lowercase).collect();
    let tag = filter.tag.as_deref().map(str::to_lowercase);
    let mut list = Vec::new();
    for (memo, summary) in memos(store)? {
        if summary.archived != filter.archived
            || tag.as_ref().is_some_and(|t| !summary.tags.contains(t))
            || filter.created_from.is_some_and(|from| summary.created_at < from)
            || filter.created_before.is_some_and(|before| summary.created_at >= before)
        {
            continue;
        }
        let body = shown(&memo).map_or("", |p| p.body.as_str());
        if !terms.is_empty() {
            let body = body.to_lowercase();
            if !terms.iter().all(|t| body.contains(t.as_str())) {
                continue;
            }
        }
        if !filter.has.is_empty() {
            let contents = text::contents(body);
            if !filter.has.iter().all(|has| has.matches(&contents, summary.images)) {
                continue;
            }
        }
        list.push(summary);
    }
    list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated_at.cmp(&a.updated_at)).then(b.id.cmp(&a.id)));
    Ok(list)
}

/// Every memo the UI can show, with its summary.
fn memos<S: Store>(store: &S) -> Result<Vec<(MemoRecord, MemoSummary)>> {
    let mut memos = Vec::new();
    for id in store.memo_ids()? {
        if let Some(memo) = store.memo(id)?.filter(|m| !m.pending_delete) {
            let summary = summarize(store, &memo)?;
            memos.push((memo, summary));
        }
    }
    Ok(memos)
}

/// When the memos in the main list (not archived) were created, oldest first, for a calendar.
pub fn activity<S: Store>(store: &S) -> Result<Vec<i64>> {
    let mut times: Vec<i64> =
        memos(store)?.into_iter().filter(|(_, s)| !s.archived).map(|(_, s)| s.created_at).collect();
    times.sort_unstable();
    Ok(times)
}

pub fn stats<S: Store>(store: &S) -> Result<Stats> {
    let mut stats = Stats::default();
    let mut tags = std::collections::BTreeSet::new();
    for (memo, summary) in memos(store)? {
        if summary.archived {
            stats.archived += 1;
            continue;
        }
        let contents = text::contents(shown(&memo).map_or("", |p| p.body.as_str()));
        stats.memos += 1;
        stats.words += contents.words;
        stats.images += summary.images;
        stats.open_tasks += contents.open_tasks;
        stats.done_tasks += contents.done_tasks;
        stats.shared += usize::from(summary.shared);
        stats.since = Some(stats.since.map_or(summary.created_at, |s| s.min(summary.created_at)));
        tags.extend(summary.tags);
    }
    stats.tags = tags.len();
    Ok(stats)
}

/// The saved filters from the vault, by name.
pub fn saved_filters<S: Store>(store: &S) -> Result<Vec<SavedFilter>> {
    let Some(vault) = store.vault()? else { return Ok(Vec::new()) };
    let mut filters: Vec<SavedFilter> = vault
        .current()
        .settings
        .iter()
        .filter_map(|(key, value)| {
            let id = Uuid::try_parse(key.strip_prefix(FILTER_PREFIX)?).ok()?;
            let filter: SavedFilter = serde_json::from_value(value.clone()).ok()?;
            Some(SavedFilter { id, ..filter })
        })
        .collect();
    filters.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    Ok(filters)
}

/// Tags of the memos in the main list (not archived), most used first.
pub fn tag_counts<S: Store>(store: &S) -> Result<Vec<TagCount>> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for summary in memo_list(store, &MemoFilter::default())? {
        for tag in summary.tags {
            *counts.entry(tag).or_default() += 1;
        }
    }
    let mut tags: Vec<TagCount> = counts.into_iter().map(|(tag, count)| TagCount { tag, count }).collect();
    tags.sort_by(|a, b| b.count.cmp(&a.count).then(a.tag.cmp(&b.tag)));
    Ok(tags)
}

pub fn memo_detail<S: Store>(store: &S, user_id: Uuid, id: Uuid) -> Result<MemoDetail> {
    let memo = store.memo(id)?.filter(|m| !m.pending_delete).ok_or(Error::NotFound)?;
    let summary = summarize(store, &memo)?;
    let payload = shown(&memo);
    let access = store.access(id)?;
    let mut members = Vec::new();
    for member in access.iter().flat_map(|a| &a.members) {
        let (trust, key) = trust(store, user_id, member.user_id, member.public_key)?;
        members.push(MemberView {
            user_id: member.user_id,
            username: member.username.clone(),
            permission: member.permission,
            trust,
            fingerprint: fingerprint(member.user_id, &key),
        });
    }
    let mut attachments: Vec<AttachmentView> = Vec::new();
    // The other side of a conflict may have images this side does not.
    let theirs = memo.conflict.iter().flat_map(|c| &c.theirs.attachments);
    for attachment in payload.iter().flat_map(|p| &p.attachments).chain(theirs) {
        if attachments.iter().any(|a| a.id == attachment.id) {
            continue;
        }
        let record = store.attachment(attachment.id)?;
        let cached = |kind| store.blob(attachment.id, kind).map(|b| b.is_some());
        attachments.push(AttachmentView {
            id: attachment.id,
            mime: attachment.mime.clone(),
            size: attachment.size,
            width: attachment.width,
            height: attachment.height,
            thumb: attachment.thumb.clone(),
            available: record.as_ref().is_some_and(|r| r.uploaded) || cached(crate::AttachmentKind::Original)?,
            thumb_available: record.as_ref().is_some_and(|r| r.thumb_uploaded)
                || cached(crate::AttachmentKind::Thumbnail)?,
        });
    }
    let mut linked_from = Vec::new();
    for other in store.memo_ids()? {
        let Some(other) = store.memo(other)?.filter(|m| !m.pending_delete && m.id != id) else { continue };
        if shown(&other).is_some_and(|p| text::linked_memos(&p.body).contains(&id)) {
            linked_from.push(MemoLink { id: other.id, title: text::title(shown(&other).map_or("", |p| &p.body)) });
        }
    }
    linked_from.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then(a.id.cmp(&b.id)));
    Ok(MemoDetail {
        linked_from,
        body: payload.map(|p| p.body.clone()).unwrap_or_default(),
        edited_at: payload.map_or(0, |p| p.edited_at),
        server_version: memo.server_version,
        updated_by: memo.updated_by,
        own: memo.owner_id == user_id,
        members,
        attachments,
        conflict_with: memo.conflict.as_ref().map(|c| ConflictView {
            body: c.theirs.body.clone(),
            version: c.theirs_version,
            edited_at: c.theirs.edited_at,
        }),
        summary,
    })
}

/// How far the key a server presents for `user_id` can be trusted, and the key to show the
/// fingerprint of. For the share dialog, before anything is pinned.
pub fn contact_trust<S: Store>(store: &S, user_id: Uuid, user: &UserInfo) -> Result<(Trust, String)> {
    let (trust, key) = trust(store, user_id, user.user_id, user.public_key)?;
    Ok((trust, fingerprint(user.user_id, &key)))
}

fn trust<S: Store>(store: &S, me: Uuid, user_id: Uuid, presented: PublicKey) -> Result<(Trust, PublicKey)> {
    if user_id == me {
        return Ok((Trust::Me, presented));
    }
    let contact = store.vault()?.and_then(|v| v.current().contacts.get(&user_id).cloned());
    Ok(match contact {
        None => (Trust::NotPinned, presented),
        Some(c) if c.public_key != presented => (Trust::Changed, c.public_key),
        Some(c) if c.verified => (Trust::Verified, c.public_key),
        Some(c) => (Trust::Pinned, c.public_key),
    })
}

/// Decrypts a revision from `GET /memos/{id}/revisions/{version}` with the cached key.
pub fn open_revision<S: Store>(store: &S, memo_id: Uuid, revision: &Revision) -> Result<MemoPayload> {
    let access = store.access(memo_id)?.ok_or(Error::NotFound)?;
    let key = access.keys.get(&revision.key_version).ok_or(Error::Decrypt)?;
    let ctx = MemoContext { memo_id, version: revision.version, key_version: revision.key_version };
    open_memo(key, &ctx, &revision.ciphertext)
}

/// The payload to show: the working copy, or the read-only view of an undecodable one.
fn shown(memo: &MemoRecord) -> Option<&MemoPayload> {
    memo.current().or_else(|| memo.undecoded.as_ref().and_then(|u| u.view.as_ref()))
}

fn summarize<S: Store>(store: &S, memo: &MemoRecord) -> Result<MemoSummary> {
    let payload = shown(memo);
    let body = payload.map_or("", |p| p.body.as_str());
    let state = store.state(memo.id)?;
    let access = store.access(memo.id)?;
    let writable = access.as_ref().is_some_and(|a| a.permission == Permission::Write)
        && memo.push_error != Some(PushError::Forbidden)
        && memo.undecoded.as_ref().is_none_or(|u| u.reason != UndecodedReason::NewerFormat);
    Ok(MemoSummary {
        id: memo.id,
        owner_id: memo.owner_id,
        title: text::title(body),
        preview: text::preview(body),
        tags: text::tags(body),
        pinned: state.as_ref().is_some_and(|s| s.pinned),
        archived: state.as_ref().is_some_and(|s| s.archived),
        shared: access.as_ref().is_some_and(|a| a.members.iter().any(|m| m.user_id != memo.owner_id)),
        writable,
        created_at: memo.created_at,
        updated_at: memo.updated_at.max(memo.local.as_ref().map_or(0, |p| p.edited_at)),
        dirty: memo.is_dirty(),
        conflict: memo.conflict.is_some(),
        undecoded: memo.undecoded.as_ref().map(|u| u.reason),
        push_error: memo.push_error,
        images: payload.map_or(0, |p| p.attachments.len()),
    })
}
