//! The client side of sync: local state, the store trait, the sync engine, local edits and
//! sharing. See `docs/sync.md`.

mod engine;
mod local;
mod memory;
mod ops;
mod sharing;
mod store;
#[cfg(test)]
mod tests;
mod view;

pub use engine::{
    CopyReason, MAX_ROUNDS, Step, SyncEvent, SyncReport, SyncSession, Upload, pending_uploads, upload_done,
};
pub use local::{
    NewAttachment, NewThumbnail, add_attachment, cached_attachment, create_memo, delete_memo, edit_body, edit_memo,
    has_unsynced, memo_attachment, resolve_conflict, resolve_with_body, restore_revision, save_as_copy, set_memo_state,
    toggle_task, verify_contact,
};
pub use memory::MemoryStore;
pub use sharing::{PreparedRevoke, apply_revoke, prepare_revoke, prepare_share};
pub use store::{
    AccessRecord, AttachmentRecord, Conflict, InFlight, MemoRecord, PushError, StateRecord, Store, SyncMeta, Undecoded,
    UndecodedReason, VaultRecord,
};
pub use view::{
    AttachmentView, ConflictView, MemberView, MemoDetail, MemoFilter, MemoSummary, TagCount, Trust, contact_trust,
    memo_detail, memo_list, open_revision, tag_counts,
};
