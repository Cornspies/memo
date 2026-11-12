use std::collections::BTreeMap;

use memo_core::{
    AttachmentKind, MemoKey, MemoPayload, UserKeyPair, VaultPayload,
    api::{Member, Permission},
    sync::{
        AccessRecord, AttachmentRecord, Conflict, MemoRecord, MemoryStore, PushError, StateRecord, Store, SyncMeta,
        Undecoded, UndecodedReason, VaultRecord,
    },
};
use uuid::Uuid;

use super::SqliteStore;

/// xorshift64*: deterministic, so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn coin(&mut self) -> bool {
        self.below(2) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len() as u64) as usize]
    }
}

fn payload(rng: &mut Rng) -> MemoPayload {
    MemoPayload::new(format!("body {}", rng.below(5)), rng.below(100) as i64)
}

fn memo(rng: &mut Rng, id: Uuid, owner: Uuid) -> MemoRecord {
    MemoRecord {
        id,
        owner_id: owner,
        server_version: rng.below(4),
        key_version: 1,
        base: rng.coin().then(|| payload(rng)),
        local: rng.coin().then(|| payload(rng)),
        highest_seen_version: rng.below(4),
        pending_delete: rng.below(4) == 0,
        conflict: (rng.below(4) == 0).then(|| Conflict {
            theirs: payload(rng),
            theirs_version: 3,
            theirs_key_version: 1,
        }),
        undecoded: (rng.below(4) == 0).then(|| Undecoded {
            version: 5,
            key_version: 2,
            ciphertext: vec![1, 2, 3],
            reason: UndecodedReason::MissingKey,
            view: None,
        }),
        push_error: (rng.below(5) == 0).then_some(PushError::Invalid),
        in_flight: None,
        created_at: 1,
        updated_at: rng.below(100) as i64,
        updated_by: rng.coin().then_some(owner),
    }
}

fn attachment(rng: &mut Rng, id: Uuid, memo_id: Uuid) -> AttachmentRecord {
    AttachmentRecord {
        id,
        memo_id,
        on_server: rng.coin(),
        uploaded: rng.coin(),
        thumb_uploaded: rng.coin(),
        upload_original: rng.below(3) == 0,
        upload_thumbnail: rng.below(3) == 0,
        pending_delete: rng.below(3) == 0,
    }
}

fn kind(rng: &mut Rng) -> AttachmentKind {
    if rng.coin() { AttachmentKind::Original } else { AttachmentKind::Thumbnail }
}

/// Every read the engine makes, so the two stores can be compared.
fn snapshot(s: &impl Store, ids: &[Uuid]) -> String {
    let mut out = format!("{:?}\n{:?}\n{:?}\n{:?}\n", s.meta(), s.memo_ids(), s.state_ids(), s.attachment_ids());
    out += &format!("{:?}\n{:?}\n{:?}\n", s.dirty_memo_ids(), s.undecoded_memo_ids(), s.dirty_state_ids());
    out += &format!("{:?}\n{:?}\n", s.pending_attachments(), s.vault());
    for &id in ids {
        out += &format!("{:?}\n{:?}\n{:?}\n", s.memo(id), s.access(id), s.state(id));
        out += &format!("{:?}\n{:?}\n", s.attachment(id), s.attachments_of(id));
        out += &format!("{:?}\n", s.blob(id, AttachmentKind::Original));
        out += &format!("{:?}\n", s.blob(id, AttachmentKind::Thumbnail));
    }
    out
}

#[test]
fn behaves_like_the_memory_store() {
    let mut rng = Rng(0x5eed);
    let ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
    let owner = Uuid::now_v7();
    let member_key = *UserKeyPair::generate().public_key();
    let (mut memory, mut sqlite) = (MemoryStore::new(), SqliteStore::open_in_memory().unwrap());
    let mut in_tx = false;

    for step in 0..3000 {
        let id = rng.pick(&ids);
        let other = rng.pick(&ids);
        match rng.below(16) {
            0 if !in_tx => {
                memory.begin().unwrap();
                sqlite.begin().unwrap();
                in_tx = true;
            }
            1 if in_tx => {
                memory.commit().unwrap();
                sqlite.commit().unwrap();
                in_tx = false;
            }
            2 if in_tx => {
                memory.rollback();
                sqlite.rollback();
                in_tx = false;
            }
            3 => {
                let meta = SyncMeta {
                    cursor: rng.below(1000),
                    epoch: rng.coin().then(|| "e".into()),
                    resetting: rng.coin(),
                    server_restored: rng.coin(),
                };
                memory.set_meta(&meta).unwrap();
                sqlite.set_meta(&meta).unwrap();
            }
            4 | 5 => {
                let m = memo(&mut rng, id, owner);
                memory.put_memo(&m).unwrap();
                sqlite.put_memo(&m).unwrap();
            }
            6 => {
                memory.delete_memo(id).unwrap();
                sqlite.delete_memo(id).unwrap();
            }
            7 => {
                let member = Member {
                    user_id: other,
                    username: "bob".into(),
                    public_key: member_key,
                    permission: Permission::Read,
                };
                let access = AccessRecord {
                    memo_id: id,
                    owner_id: owner,
                    permission: if rng.coin() { Permission::Write } else { Permission::Read },
                    members: vec![member],
                    keys: BTreeMap::from([(1, MemoKey::generate()), (2, MemoKey::generate())]),
                };
                memory.put_access(&access).unwrap();
                sqlite.put_access(&access).unwrap();
            }
            8 => {
                memory.delete_access(id).unwrap();
                sqlite.delete_access(id).unwrap();
            }
            9 => {
                let state = StateRecord { memo_id: id, pinned: rng.coin(), archived: rng.coin(), dirty: rng.coin() };
                memory.put_state(&state).unwrap();
                sqlite.put_state(&state).unwrap();
            }
            10 => {
                memory.delete_state(id).unwrap();
                sqlite.delete_state(id).unwrap();
            }
            11 => {
                let a = attachment(&mut rng, id, other);
                memory.put_attachment(&a).unwrap();
                sqlite.put_attachment(&a).unwrap();
            }
            12 => {
                memory.delete_attachment(id).unwrap();
                sqlite.delete_attachment(id).unwrap();
            }
            13 => {
                let mut settings = serde_json::Map::new();
                settings.insert("theme".to_owned(), serde_json::json!(rng.below(3)));
                let local = rng.coin().then(|| VaultPayload { settings: settings.clone(), ..VaultPayload::default() });
                let vault = VaultRecord {
                    server_version: rng.below(5),
                    base: VaultPayload::default(),
                    local,
                    newer_format: rng.coin(),
                };
                memory.put_vault(&vault).unwrap();
                sqlite.put_vault(&vault).unwrap();
            }
            _ => {
                let (kind, bytes) = (kind(&mut rng), vec![rng.below(256) as u8; rng.below(8) as usize]);
                memory.put_blob(id, kind, &bytes).unwrap();
                sqlite.put_blob(id, kind, &bytes).unwrap();
            }
        }
        assert_eq!(snapshot(&memory, &ids), snapshot(&sqlite, &ids), "after step {step}");
    }
}

#[test]
fn survives_reopening() {
    let dir = std::env::temp_dir().join(format!("memo-sqlite-{}", Uuid::now_v7()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("memo.db");
    let id = Uuid::now_v7();
    let m = memo(&mut Rng(7), id, id);
    {
        let mut store = SqliteStore::open(&path).unwrap();
        store.begin().unwrap();
        store.put_memo(&m).unwrap();
        store.put_blob(id, AttachmentKind::Original, b"envelope").unwrap();
        store.commit().unwrap();
        store.begin().unwrap();
        store.delete_memo(id).unwrap();
        // Dropped with the transaction still open: it must not land.
    }
    let store = SqliteStore::open(&path).unwrap();
    assert_eq!(store.memo(id).unwrap(), Some(m));
    assert_eq!(store.blob(id, AttachmentKind::Original).unwrap().as_deref(), Some(&b"envelope"[..]));
    drop(store);

    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 99).unwrap();
    drop(conn);
    assert!(SqliteStore::open(&path).is_err(), "a newer schema is not touched");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn trimming_keeps_recent_and_unsent_blobs() {
    let mut store = SqliteStore::open_in_memory().unwrap();
    let ids: Vec<Uuid> = (0..4).map(|_| Uuid::now_v7()).collect();
    for &id in &ids {
        store.put_blob(id, AttachmentKind::Original, &[0; 100]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // ids[0] is the oldest but still waiting for its upload.
    let mut unsent = attachment(&mut Rng(1), ids[0], ids[0]);
    unsent.upload_original = true;
    store.put_attachment(&unsent).unwrap();
    store.blob(ids[1], AttachmentKind::Original).unwrap(); // used again: now the newest

    assert_eq!(store.trim_blobs(250).unwrap(), 100);
    let kept: Vec<bool> = ids.iter().map(|&id| store.blob(id, AttachmentKind::Original).unwrap().is_some()).collect();
    assert_eq!(kept, [true, true, false, true]);
    assert_eq!(store.trim_blobs(0).unwrap(), 200);
    assert!(store.blob(ids[0], AttachmentKind::Original).unwrap().is_some());
}

#[test]
fn views_follow_the_store() {
    use memo_core::{AccountKey, UnlockedAccount, sync as client};
    let account = UnlockedAccount {
        user_id: Uuid::now_v7(),
        account_key: AccountKey::generate(),
        keypair: UserKeyPair::generate(),
    };
    let mut store = SqliteStore::open_in_memory().unwrap();
    let mut views = client::Views::new();
    let all = client::MemoFilter::default();
    let id = client::create_memo(&mut store, &account, MemoPayload::new("first #tag", 1), 1).unwrap();
    assert_eq!(views.memo_list(&store, &all).unwrap()[0].title, "first #tag");

    // Reads leave the generation alone, also a blob read, which notes when it was used.
    let generation = store.generation();
    store.put_blob(id, AttachmentKind::Original, b"envelope").unwrap();
    assert!(store.blob(id, AttachmentKind::Original).unwrap().is_some());
    assert_eq!(views.tag_counts(&store).unwrap().len(), 1);
    assert_eq!(store.generation(), generation);

    client::edit_body(&mut store, id, "second", 2, false).unwrap();
    assert_eq!(views.memo_list(&store, &all).unwrap()[0].title, "second");
    assert!(views.tag_counts(&store).unwrap().is_empty());

    store.begin().unwrap();
    store.delete_memo(id).unwrap();
    assert!(views.memo_list(&store, &all).unwrap().is_empty());
    store.rollback();
    assert_eq!(views.memo_list(&store, &all).unwrap().len(), 1);
}
