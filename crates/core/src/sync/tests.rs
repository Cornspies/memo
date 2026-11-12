//! End-to-end sync scenarios against an in-memory server that follows `docs/sync.md`.

use std::collections::{BTreeMap, BTreeSet};

use uuid::Uuid;

use super::*;
use crate::{
    account::UnlockedAccount,
    api::*,
    content::seal_vault,
    envelope::AttachmentKind,
    keys::{AccountKey, PublicKey, UserKeyPair},
    payload::{MemoPayload, VaultPayload},
    share::{KeyGrant, wrap_memo_key},
};

// ---- Reference server ----

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Entity {
    Memo(Uuid),
    Access(Uuid),
    Attachment(Uuid),
    State(Uuid),
    Vault,
}

#[derive(Clone, Debug)]
struct ServerMemo {
    owner: Uuid,
    version: u64,
    key_version: u32,
    ciphertext: Option<Vec<u8>>,
    updated_by: Option<Uuid>,
}

#[derive(Clone, Debug, Default)]
struct ServerAttachment {
    memo_id: Uuid,
    uploaded: bool,
    thumb_uploaded: bool,
    deleted: bool,
}

#[derive(Clone, Debug)]
struct Server {
    epoch: String,
    seq: u64,
    page_limit: usize,
    users: BTreeMap<Uuid, (String, PublicKey)>,
    memos: BTreeMap<Uuid, ServerMemo>,
    members: BTreeMap<(Uuid, Uuid), Permission>,
    keys: BTreeMap<(Uuid, Uuid, u32), Vec<u8>>,
    states: BTreeMap<(Uuid, Uuid), (bool, bool)>,
    attachments: BTreeMap<Uuid, ServerAttachment>,
    vaults: BTreeMap<Uuid, (u64, Vec<u8>)>,
    log: BTreeMap<(Uuid, Entity), u64>,
    /// One value for all users is enough here.
    purged_through: u64,
}

impl Server {
    fn new() -> Self {
        Self {
            epoch: "epoch-1".into(),
            seq: 0,
            page_limit: 500,
            users: BTreeMap::new(),
            memos: BTreeMap::new(),
            members: BTreeMap::new(),
            keys: BTreeMap::new(),
            states: BTreeMap::new(),
            attachments: BTreeMap::new(),
            vaults: BTreeMap::new(),
            log: BTreeMap::new(),
            purged_through: 0,
        }
    }

    fn register(&mut self, name: &str, account: &UnlockedAccount) {
        self.users.insert(account.user_id, (name.into(), *account.keypair.public_key()));
        let vault = seal_vault(&account.account_key, account.user_id, 1, &VaultPayload::default()).unwrap();
        self.vaults.insert(account.user_id, (1, vault));
        self.fan_out(Entity::Vault, [account.user_id]);
    }

    fn fan_out(&mut self, entity: Entity, users: impl IntoIterator<Item = Uuid>) {
        self.seq += 1;
        for user in users {
            self.log.insert((user, entity), self.seq);
        }
    }

    fn members_of(&self, memo_id: Uuid) -> Vec<Uuid> {
        self.members.keys().filter(|(m, _)| *m == memo_id).map(|(_, u)| *u).collect()
    }

    fn permission(&self, memo_id: Uuid, user: Uuid) -> Option<Permission> {
        self.members.get(&(memo_id, user)).copied()
    }

    fn sync(&mut self, user: Uuid, request: &SyncRequest) -> SyncResponse {
        let reset = request.epoch.as_ref().is_some_and(|e| *e != self.epoch)
            || (request.cursor > 0 && request.cursor < self.purged_through)
            || request.cursor > self.seq;
        if reset {
            return SyncResponse {
                results: vec![],
                changes: vec![],
                cursor: 0,
                epoch: self.epoch.clone(),
                more: false,
                reset: true,
            };
        }
        let results = request.push.iter().map(|op| self.apply(user, op)).collect();

        let mut rows: Vec<(u64, Entity)> = self
            .log
            .iter()
            .filter(|((u, _), seq)| *u == user && **seq > request.cursor)
            .map(|((_, e), s)| (*s, *e))
            .collect();
        rows.sort();
        // A full sync starts with the vault, so clients know their pins before verifying keys.
        if request.cursor == 0
            && let Some(pos) = rows.iter().position(|(_, e)| *e == Entity::Vault)
        {
            let vault = rows.remove(pos);
            rows.insert(0, vault);
        }
        let more = rows.len() > self.page_limit;
        rows.truncate(self.page_limit);
        let cursor = if more {
            rows.iter()
                .filter(|(_, e)| *e != Entity::Vault || request.cursor > 0)
                .map(|(s, _)| *s)
                .max()
                .unwrap_or(request.cursor)
        } else {
            self.log.iter().filter(|((u, _), _)| *u == user).map(|(_, s)| *s).max().unwrap_or(0).max(request.cursor)
        };
        let changes = rows.into_iter().filter_map(|(seq, entity)| self.render(user, seq, entity)).collect();
        SyncResponse { results, changes, cursor, epoch: self.epoch.clone(), more, reset: false }
    }

    /// `None` for a live memo the user is not a member of.
    fn render(&self, user: Uuid, seq: u64, entity: Entity) -> Option<Change> {
        Some(match entity {
            Entity::Memo(id) => {
                let m = &self.memos[&id];
                if m.ciphertext.is_some() && self.permission(id, user).is_none() {
                    return None;
                }
                Change::Memo(MemoChange {
                    seq,
                    id,
                    owner_id: m.owner,
                    version: m.version,
                    key_version: m.key_version,
                    deleted: m.ciphertext.is_none(),
                    ciphertext: m.ciphertext.clone(),
                    created_at: 0,
                    updated_at: 0,
                    updated_by: m.updated_by,
                })
            }
            Entity::Access(memo_id) => match self.permission(memo_id, user) {
                None => Change::Access(AccessChange {
                    seq,
                    memo_id,
                    revoked: true,
                    owner_id: None,
                    permission: None,
                    keys: vec![],
                    members: vec![],
                }),
                Some(permission) => Change::Access(AccessChange {
                    seq,
                    memo_id,
                    revoked: false,
                    owner_id: Some(self.memos[&memo_id].owner),
                    permission: Some(permission),
                    keys: self
                        .keys
                        .iter()
                        .filter(|((m, u, _), _)| *m == memo_id && *u == user)
                        .map(|((_, _, kv), wk)| WrappedKey { key_version: *kv, wrapped_key: wk.clone() })
                        .collect(),
                    members: self
                        .members_of(memo_id)
                        .into_iter()
                        .map(|u| Member {
                            user_id: u,
                            username: self.users[&u].0.clone(),
                            public_key: self.users[&u].1,
                            permission: self.members[&(memo_id, u)],
                        })
                        .collect(),
                }),
            },
            Entity::Attachment(id) => {
                let a = &self.attachments[&id];
                Change::Attachment(AttachmentChange {
                    seq,
                    id,
                    memo_id: a.memo_id,
                    size: None,
                    thumb_size: None,
                    uploaded: a.uploaded,
                    thumb_uploaded: a.thumb_uploaded,
                    deleted: a.deleted,
                })
            }
            Entity::State(memo_id) => {
                let (pinned, archived) = self.states[&(memo_id, user)];
                Change::State(StateChange { seq, memo_id, pinned, archived })
            }
            Entity::Vault => {
                let (version, ciphertext) = self.vaults[&user].clone();
                Change::Vault(VaultChange { seq, version, ciphertext })
            }
        })
    }

    fn apply(&mut self, user: Uuid, op: &PushOp) -> OpResult {
        let ok = |version| OpResult { status: OpStatus::Ok, version, current_version: None };
        match op {
            PushOp::MemoPut { id, base_version, key_version, ciphertext, wrapped_key } => {
                let Some(memo) = self.memos.get(id) else {
                    if *base_version != 0 || *key_version != 1 || wrapped_key.is_none() {
                        return OpResult::status(if *base_version != 0 {
                            OpStatus::NotFound
                        } else {
                            OpStatus::Invalid
                        });
                    }
                    let memo = ServerMemo {
                        owner: user,
                        version: 1,
                        key_version: 1,
                        ciphertext: Some(ciphertext.clone()),
                        updated_by: Some(user),
                    };
                    self.memos.insert(*id, memo);
                    self.members.insert((*id, user), Permission::Write);
                    self.keys.insert((*id, user, 1), wrapped_key.clone().unwrap());
                    self.fan_out(Entity::Access(*id), [user]);
                    self.fan_out(Entity::Memo(*id), [user]);
                    return ok(Some(1));
                };
                if memo.ciphertext.is_none() && self.log.contains_key(&(user, Entity::Memo(*id))) {
                    return OpResult::status(OpStatus::Deleted);
                }
                let Some(permission) = self.permission(*id, user) else { return OpResult::status(OpStatus::NotFound) };
                let current = Some(memo.version);
                if *base_version == 0 {
                    return OpResult { status: OpStatus::Conflict, version: None, current_version: current };
                }
                if permission != Permission::Write {
                    return OpResult::status(OpStatus::Forbidden);
                }
                if *base_version != memo.version || *key_version != memo.key_version {
                    return OpResult { status: OpStatus::Conflict, version: None, current_version: current };
                }
                let memo = self.memos.get_mut(id).unwrap();
                memo.version += 1;
                memo.ciphertext = Some(ciphertext.clone());
                memo.updated_by = Some(user);
                let version = memo.version;
                self.fan_out(Entity::Memo(*id), self.members_of(*id));
                ok(Some(version))
            }
            PushOp::MemoDelete { id, base_version } => {
                let Some(memo) = self.memos.get(id).cloned() else { return OpResult::status(OpStatus::NotFound) };
                if memo.ciphertext.is_none() {
                    return ok(None);
                }
                if self.permission(*id, user).is_none() {
                    return OpResult::status(OpStatus::NotFound);
                }
                if memo.owner != user {
                    return OpResult::status(OpStatus::Forbidden);
                }
                if *base_version != memo.version {
                    return OpResult { status: OpStatus::Conflict, version: None, current_version: Some(memo.version) };
                }
                let former = self.members_of(*id);
                self.memos.get_mut(id).unwrap().ciphertext = None;
                self.members.retain(|(m, _), _| m != id);
                self.keys.retain(|(m, _, _), _| m != id);
                self.states.retain(|(m, _), _| m != id);
                for (attachment_id, a) in self.attachments.iter_mut().filter(|(_, a)| a.memo_id == *id) {
                    a.deleted = true;
                    self.seq += 1;
                    for u in &former {
                        self.log.insert((*u, Entity::Attachment(*attachment_id)), self.seq);
                    }
                }
                self.fan_out(Entity::Memo(*id), former);
                ok(None)
            }
            PushOp::StatePut { memo_id, pinned, archived } => {
                if self.permission(*memo_id, user).is_none() {
                    return OpResult::status(OpStatus::NotFound);
                }
                self.states.insert((*memo_id, user), (*pinned, *archived));
                self.fan_out(Entity::State(*memo_id), [user]);
                ok(None)
            }
            PushOp::AttachmentDelete { id } => {
                let Some(a) = self.attachments.get(id).cloned() else { return ok(None) };
                if self.permission(a.memo_id, user) != Some(Permission::Write) {
                    return OpResult::status(OpStatus::Forbidden);
                }
                self.attachments.get_mut(id).unwrap().deleted = true;
                self.fan_out(Entity::Attachment(*id), self.members_of(a.memo_id));
                ok(None)
            }
            PushOp::VaultPut { base_version, ciphertext } => {
                let (version, _) = self.vaults[&user];
                if *base_version != version {
                    return OpResult { status: OpStatus::Conflict, version: None, current_version: Some(version) };
                }
                self.vaults.insert(user, (version + 1, ciphertext.clone()));
                self.fan_out(Entity::Vault, [user]);
                ok(Some(version + 1))
            }
        }
    }

    fn add_member(&mut self, owner: Uuid, memo_id: Uuid, request: &AddMemberRequest) {
        assert_eq!(self.memos[&memo_id].owner, owner);
        self.members.insert((memo_id, request.user_id), request.permission);
        for key in &request.keys {
            self.keys.insert((memo_id, request.user_id, key.key_version), key.wrapped_key.clone());
        }
        self.fan_out(Entity::Access(memo_id), self.members_of(memo_id));
        self.fan_out(Entity::Memo(memo_id), [request.user_id]);
    }

    fn revoke(&mut self, owner: Uuid, memo_id: Uuid, request: &RevokeRequest) -> RevokeResponse {
        let memo = self.memos.get_mut(&memo_id).unwrap();
        assert_eq!(memo.owner, owner);
        assert_eq!(memo.version, request.base_version);
        memo.version += 1;
        memo.key_version = request.new_key_version;
        memo.ciphertext = Some(request.ciphertext.clone());
        let version = memo.version;
        let everyone = self.members_of(memo_id);
        self.members.retain(|(m, u), _| *m != memo_id || !request.user_ids.contains(u));
        self.keys.retain(|(m, u, _), _| *m != memo_id || !request.user_ids.contains(u));
        for key in &request.keys {
            self.keys.insert((memo_id, key.user_id, request.new_key_version), key.wrapped_key.clone());
        }
        self.log.retain(|(u, e), _| {
            !(request.user_ids.contains(u) && matches!(e, Entity::Memo(m) | Entity::State(m) if *m == memo_id))
        });
        self.fan_out(Entity::Access(memo_id), everyone);
        self.fan_out(Entity::Memo(memo_id), self.members_of(memo_id));
        RevokeResponse { version }
    }

    fn upload(&mut self, upload: &Upload) {
        let a = self
            .attachments
            .entry(upload.attachment_id)
            .or_insert(ServerAttachment { memo_id: upload.memo_id, ..Default::default() });
        match upload.kind {
            AttachmentKind::Original => a.uploaded = true,
            AttachmentKind::Thumbnail => a.thumb_uploaded = true,
        }
        self.fan_out(Entity::Attachment(upload.attachment_id), self.members_of(upload.memo_id));
    }
}

// ---- Devices ----

struct Device {
    store: MemoryStore,
    account: UnlockedAccount,
}

impl Device {
    fn new(account: &UnlockedAccount) -> Self {
        Self { store: MemoryStore::new(), account: account.clone() }
    }

    fn sync(&mut self, server: &mut Server) -> Vec<SyncEvent> {
        let mut session = SyncSession::start(&mut self.store, &self.account, 1000).unwrap();
        loop {
            let response = server.sync(self.account.user_id, session.request());
            if session.handle(&mut self.store, &self.account, response).unwrap() == Step::Done {
                break;
            }
        }
        for upload in pending_uploads(&self.store).unwrap() {
            assert!(self.store.blob(upload.attachment_id, upload.kind).unwrap().is_some());
            server.upload(&upload);
            upload_done(&mut self.store, upload.attachment_id, upload.kind).unwrap();
        }
        session.into_report().events
    }

    fn create(&mut self, body: &str) -> Uuid {
        create_memo(&mut self.store, &self.account, MemoPayload::new(body, 1), 1).unwrap()
    }

    fn edit(&mut self, id: Uuid, body: &str) {
        let mut payload = self.memo(id).current().unwrap().clone();
        payload.body = body.into();
        edit_memo(&mut self.store, id, payload).unwrap();
    }

    fn memo(&self, id: Uuid) -> MemoRecord {
        self.store.memo(id).unwrap().expect("memo exists")
    }

    fn body(&self, id: Uuid) -> String {
        self.memo(id).current().unwrap().body.clone()
    }

    fn memo_ids(&self) -> Vec<Uuid> {
        self.store.memo_ids().unwrap()
    }
}

fn user(server: &mut Server, name: &str) -> UnlockedAccount {
    let account = UnlockedAccount {
        user_id: Uuid::now_v7(),
        account_key: AccountKey::generate(),
        keypair: UserKeyPair::generate(),
    };
    server.register(name, &account);
    account
}

fn clean(device: &Device, id: Uuid) -> bool {
    !device.memo(id).is_dirty()
}

// ---- Scenarios ----

#[test]
fn second_device_receives_memos() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("hello");
    assert!(a1.sync(&mut server).is_empty());
    assert!(clean(&a1, id));
    assert_eq!(a1.memo(id).server_version, 1);

    assert!(a2.sync(&mut server).is_empty());
    assert_eq!(a2.body(id), "hello");
    assert!(clean(&a2, id));
    assert_eq!(server.memos[&id].version, 1);
}

#[test]
fn concurrent_edits_merge() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("one\ntwo\nthree\n");
    a1.sync(&mut server);
    a2.sync(&mut server);

    a1.edit(id, "ONE\ntwo\nthree\n");
    a2.edit(id, "one\ntwo\nTHREE\n");
    a1.sync(&mut server);
    assert!(a2.sync(&mut server).is_empty());
    assert_eq!(a2.body(id), "ONE\ntwo\nTHREE\n");
    assert!(clean(&a2, id), "the merge was pushed in the same session");
    a1.sync(&mut server);
    assert_eq!(a1.body(id), "ONE\ntwo\nTHREE\n");
    assert_eq!(server.memos[&id].version, 3);
}

#[test]
fn overlapping_edits_wait_for_the_user() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("shopping");
    a1.sync(&mut server);
    a2.sync(&mut server);

    a1.edit(id, "shopping: milk");
    a2.edit(id, "shopping: eggs");
    a1.sync(&mut server);
    assert_eq!(a2.sync(&mut server), vec![SyncEvent::Conflict { memo_id: id }]);
    let conflict = a2.memo(id).conflict.unwrap();
    assert_eq!(conflict.theirs.body, "shopping: milk");
    assert_eq!(a2.body(id), "shopping: eggs");
    assert_eq!(edit_memo(&mut a2.store, id, MemoPayload::new("x", 2)), Err(crate::Error::InConflict));

    let mut resolved = conflict.theirs.clone();
    resolved.body = "shopping: milk, eggs".into();
    resolve_conflict(&mut a2.store, id, resolved).unwrap();
    a2.sync(&mut server);
    a1.sync(&mut server);
    assert_eq!(a1.body(id), "shopping: milk, eggs");
    assert!(clean(&a1, id) && clean(&a2, id));
}

#[test]
fn lost_response_is_recognised_as_our_own_write() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let mut a1 = Device::new(&alice);
    let id = a1.create("draft");
    a1.sync(&mut server);

    a1.edit(id, "draft two");
    let session = SyncSession::start(&mut a1.store, &a1.account, 1000).unwrap();
    server.sync(alice.user_id, session.request()); // applied, but the response never arrives
    drop(session);
    a1.edit(id, "draft two, continued");

    assert!(a1.sync(&mut server).is_empty(), "no conflict with ourselves");
    assert!(clean(&a1, id));
    assert_eq!(a1.memo(id).server_version, 3);
    let mut a2 = Device::new(&alice);
    a2.sync(&mut server);
    assert_eq!(a2.body(id), "draft two, continued");
}

#[test]
fn lost_create_response_does_not_duplicate() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let mut a1 = Device::new(&alice);
    let id = a1.create("new");
    let session = SyncSession::start(&mut a1.store, &a1.account, 1000).unwrap();
    server.sync(alice.user_id, session.request());
    drop(session);
    assert!(a1.sync(&mut server).is_empty());
    assert!(clean(&a1, id));
    assert_eq!(server.memos.len(), 1);
    assert_eq!(server.memos[&id].version, 1);
}

fn share(server: &mut Server, owner: &mut Device, id: Uuid, with: &UnlockedAccount, permission: Permission) {
    let info = UserInfo {
        user_id: with.user_id,
        username: server.users[&with.user_id].0.clone(),
        public_key: *with.keypair.public_key(),
    };
    let request = prepare_share(&mut owner.store, &owner.account, id, &info, permission, 5).unwrap();
    server.add_member(owner.account.user_id, id, &request);
}

#[test]
fn share_edit_and_revoke() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let (mut a, mut b) = (Device::new(&alice), Device::new(&bob));
    let id = a.create("plans");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Write);
    a.sync(&mut server);

    assert!(b.sync(&mut server).is_empty());
    assert_eq!(b.body(id), "plans");
    let pins = b.store.vault().unwrap().unwrap().current().contacts.clone();
    assert_eq!(pins[&alice.user_id].public_key, *alice.keypair.public_key(), "bob pinned alice on first use");
    assert!(b.store.vault().unwrap().unwrap().local.is_none(), "and synced the pin");

    b.edit(id, "plans\nbob was here");
    b.sync(&mut server);
    a.sync(&mut server);
    assert_eq!(a.body(id), "plans\nbob was here");

    // Bob edits offline while Alice revokes him.
    b.edit(id, "plans\nbob was here\nmore");
    let prepared = prepare_revoke(&a.store, &a.account, id, &[bob.user_id]).unwrap();
    let response = server.revoke(alice.user_id, id, &prepared.request);
    apply_revoke(&mut a.store, prepared, response).unwrap();
    a.sync(&mut server);
    assert_eq!(a.memo(id).key_version, 2);
    a.edit(id, "plans (private again)");
    a.sync(&mut server);
    assert!(clean(&a, id));

    let events = b.sync(&mut server);
    let [SyncEvent::RestoredCopy { original, copy, reason: CopyReason::AccessRevoked }] = events[..] else {
        panic!("{events:?}")
    };
    assert_eq!(original, id);
    assert!(b.store.memo(id).unwrap().is_none());
    assert_eq!(b.body(copy), "plans\nbob was here\nmore");
    assert!(clean(&b, copy), "the copy was pushed as bob's own memo");
    assert_eq!(server.memos[&copy].owner, bob.user_id);
}

#[test]
fn read_only_members_cannot_edit() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let (mut a, mut b) = (Device::new(&alice), Device::new(&bob));
    let id = a.create("read me");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Read);
    b.sync(&mut server);
    assert_eq!(b.body(id), "read me");
    assert_eq!(edit_memo(&mut b.store, id, MemoPayload::new("nope", 2)), Err(crate::Error::ReadOnly));
    assert_eq!(delete_memo(&mut b.store, &b.account, id), Err(crate::Error::NotOwner));
    let copy = save_as_copy(&mut b.store, &b.account, id, 9).unwrap();
    b.sync(&mut server);
    assert_eq!(server.memos[&copy].owner, bob.user_id);
}

#[test]
fn forged_keys_are_rejected() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let (mut a, mut b) = (Device::new(&alice), Device::new(&bob));
    let id = a.create("secret");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Write);
    b.sync(&mut server);

    // The server invents a memo "from alice" with a key it wrapped itself.
    let mallory = UserKeyPair::generate();
    let fake = Uuid::now_v7();
    let fake_key = crate::keys::MemoKey::generate();
    let grant = KeyGrant { memo_id: fake, key_version: 1, owner_id: alice.user_id, member_id: bob.user_id };
    let ctx = crate::content::MemoContext { memo_id: fake, version: 1, key_version: 1 };
    let ciphertext = crate::content::seal_memo(&fake_key, &ctx, &MemoPayload::new("click here", 1)).unwrap();
    server.memos.insert(
        fake,
        ServerMemo { owner: alice.user_id, version: 1, key_version: 1, ciphertext: Some(ciphertext), updated_by: None },
    );
    server.members.insert((fake, alice.user_id), Permission::Write);
    server.members.insert((fake, bob.user_id), Permission::Write);
    server.keys.insert(
        (fake, bob.user_id, 1),
        wrap_memo_key(&mallory, b.account.keypair.public_key(), &grant, &fake_key).unwrap(),
    );
    server.fan_out(Entity::Access(fake), [bob.user_id]);
    server.fan_out(Entity::Memo(fake), [bob.user_id]);

    let events = b.sync(&mut server);
    assert_eq!(events, vec![SyncEvent::KeyRejected { memo_id: fake, owner_id: alice.user_id }]);
    let memo = b.memo(fake);
    assert!(memo.base.is_none());
    assert_eq!(memo.undecoded.unwrap().reason, UndecodedReason::MissingKey);

    // The server now claims alice has another public key: the pin wins and the alarm goes off.
    server.users.get_mut(&alice.user_id).unwrap().1 = *mallory.public_key();
    server.fan_out(Entity::Access(id), [bob.user_id]);
    assert_eq!(b.sync(&mut server), vec![SyncEvent::KeyChanged { user_id: alice.user_id }]);
    let info = UserInfo { user_id: alice.user_id, username: "alice".into(), public_key: *mallory.public_key() };
    assert_eq!(
        prepare_share(&mut b.store, &b.account, Uuid::now_v7(), &info, Permission::Read, 1).unwrap_err(),
        crate::Error::NotFound
    );
    let own = b.create("mine");
    b.sync(&mut server);
    assert_eq!(
        prepare_share(&mut b.store, &b.account, own, &info, Permission::Read, 1).unwrap_err(),
        crate::Error::KeyChanged(alice.user_id)
    );
}

#[test]
fn delete_and_edit_race() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let first = a1.create("first");
    let second = a1.create("second");
    a1.sync(&mut server);
    a2.sync(&mut server);

    // Delete wins on the server, the offline edit becomes a copy.
    delete_memo(&mut a1.store, &a1.account, first).unwrap();
    a2.edit(first, "first, edited");
    a1.sync(&mut server);
    assert!(a1.store.memo(first).unwrap().is_none());
    let events = a2.sync(&mut server);
    let [SyncEvent::RestoredCopy { copy, reason: CopyReason::Deleted, .. }] = events[..] else { panic!("{events:?}") };
    assert_eq!(a2.body(copy), "first, edited");

    // The edit wins on the server, the delete is cancelled.
    a2.edit(second, "second, edited");
    a2.sync(&mut server);
    delete_memo(&mut a1.store, &a1.account, second).unwrap();
    assert_eq!(a1.sync(&mut server), vec![SyncEvent::DeleteRejected { memo_id: second }]);
    assert_eq!(a1.body(second), "second, edited");
    assert!(server.memos[&second].ciphertext.is_some());
}

#[test]
fn reset_after_restore_pushes_lost_data_back() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let kept = a1.create("kept");
    a1.sync(&mut server);
    let backup = server.clone();

    a1.edit(kept, "kept, newer");
    let lost = a1.create("only after the backup");
    a1.sync(&mut server);
    a2.sync(&mut server);

    server = Server { epoch: "epoch-2".into(), ..backup };
    let events = a1.sync(&mut server);
    assert!(events.contains(&SyncEvent::Reset));
    assert!(events.contains(&SyncEvent::ServerLostData { memo_id: kept }));
    assert_eq!(a1.body(kept), "kept, newer");
    assert!(clean(&a1, kept) && clean(&a1, lost));
    assert!(server.memos.contains_key(&lost), "unknown to the server, so it went back up as a memo");

    // a2 had the same newer data; after its reset everything agrees.
    let events = a2.sync(&mut server);
    assert!(events.contains(&SyncEvent::Reset));
    assert_eq!(a2.body(kept), "kept, newer");
    assert!(clean(&a2, kept));
    assert_eq!(a2.memo_ids().len(), 2);
}

#[test]
fn reset_after_restore_recreates_own_memos() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let mut a1 = Device::new(&alice);
    a1.sync(&mut server);
    let backup = server.clone();
    let id = a1.create("synced after the backup");
    a1.sync(&mut server);
    let unsynced = a1.create("never synced");

    server = Server { epoch: "epoch-2".into(), ..backup };
    let events = a1.sync(&mut server);
    assert!(events.contains(&SyncEvent::ServerLostData { memo_id: id }));
    assert_eq!(server.memos[&id].version, 1, "same id, created again");
    assert_eq!(a1.body(id), "synced after the backup");
    assert!(clean(&a1, id) && clean(&a1, unsynced));
}

#[test]
fn reset_after_long_offline_drops_purged_memos() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let (gone, edited, kept) = (a1.create("gone"), a1.create("edited"), a1.create("kept"));
    a1.sync(&mut server);
    a2.sync(&mut server);

    delete_memo(&mut a1.store, &a1.account, gone).unwrap();
    delete_memo(&mut a1.store, &a1.account, edited).unwrap();
    a1.sync(&mut server);
    a2.edit(edited, "edited offline");
    // Months later the tombstones are purged.
    server.log.retain(|(_, e), _| *e != Entity::Memo(gone) && *e != Entity::Memo(edited));
    server.purged_through = server.seq;
    server.fan_out(Entity::Vault, [alice.user_id]);

    let events = a2.sync(&mut server);
    assert!(events.contains(&SyncEvent::Reset));
    assert!(a2.store.memo(gone).unwrap().is_none());
    assert!(a2.store.memo(edited).unwrap().is_none());
    let copy = events.iter().find_map(|e| match e {
        SyncEvent::RestoredCopy { original, copy, .. } if *original == edited => Some(*copy),
        _ => None,
    });
    assert_eq!(a2.body(copy.unwrap()), "edited offline");
    assert_eq!(a2.body(kept), "kept");
}

#[test]
fn paging() {
    let mut server = Server::new();
    server.page_limit = 2;
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    for i in 0..5 {
        a1.create(&format!("memo {i}"));
    }
    a1.sync(&mut server);
    a2.sync(&mut server);
    assert_eq!(a2.memo_ids().len(), 5);
    assert!(a2.memo_ids().iter().all(|id| a2.memo(*id).base.is_some()));
    assert_eq!(a2.store.meta().unwrap().cursor, server.seq);
}

#[test]
fn attachments_upload_and_release() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("photo");
    let thumbnail = NewThumbnail { mime: "image/webp".into(), width: 2, height: 1, bytes: b"thumb" };
    let new = NewAttachment {
        mime: "image/jpeg".into(),
        width: 4,
        height: 2,
        bytes: b"jpeg bytes",
        thumbnail: Some(thumbnail),
    };
    let attachment = add_attachment(&mut a1.store, id, new).unwrap();
    let mut payload = a1.memo(id).current().unwrap().clone();
    assert_eq!(payload.attachments, vec![attachment.clone()]);
    payload.body = format!("![](attachment:{})", attachment.id);
    edit_memo(&mut a1.store, id, payload.clone()).unwrap();

    assert!(pending_uploads(&a1.store).unwrap().is_empty(), "not before the memo exists on the server");
    a1.sync(&mut server);
    assert!(server.attachments[&attachment.id].uploaded && server.attachments[&attachment.id].thumb_uploaded);
    assert!(pending_uploads(&a1.store).unwrap().is_empty());

    a2.sync(&mut server);
    let record = a2.store.attachment(attachment.id).unwrap().unwrap();
    assert!(record.uploaded && record.thumb_uploaded);
    assert_eq!(cached_attachment(&a2.store, &attachment, AttachmentKind::Original).unwrap(), None);
    let envelope = a1.store.blob(attachment.id, AttachmentKind::Original).unwrap().unwrap(); // "download"
    a2.store.put_blob(attachment.id, AttachmentKind::Original, &envelope).unwrap();
    assert_eq!(cached_attachment(&a2.store, &attachment, AttachmentKind::Original).unwrap().unwrap(), b"jpeg bytes");

    payload.attachments.clear();
    payload.body = "no photo".into();
    edit_memo(&mut a1.store, id, payload).unwrap();
    a1.sync(&mut server);
    assert!(server.attachments[&attachment.id].deleted);
    assert!(a1.store.attachment(attachment.id).unwrap().is_none());
    a2.sync(&mut server);
    assert!(a2.store.attachment(attachment.id).unwrap().is_none());
    assert!(a2.store.blob(attachment.id, AttachmentKind::Original).unwrap().is_none());
}

#[test]
fn states_and_vault_settings_sync() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("pin me");
    set_memo_state(&mut a1.store, id, true, false).unwrap();
    a1.sync(&mut server);
    a2.sync(&mut server);
    assert_eq!(
        a2.store.state(id).unwrap().unwrap(),
        StateRecord { memo_id: id, pinned: true, archived: false, dirty: false }
    );

    // Both devices change the vault: contacts union, settings per key.
    let bob = user(&mut server, "bob");
    let carol = user(&mut server, "carol");
    let mut vault = a1.store.vault().unwrap().unwrap();
    let mut local = vault.current().clone();
    local.settings.insert("theme".into(), "dark".into());
    vault.local = Some(local);
    a1.store.put_vault(&vault).unwrap();
    let info = |u: &UnlockedAccount, name: &str| UserInfo {
        user_id: u.user_id,
        username: name.into(),
        public_key: *u.keypair.public_key(),
    };
    prepare_share(&mut a1.store, &a1.account, id, &info(&bob, "bob"), Permission::Read, 1).unwrap();
    let other = a2.create("other");
    a2.sync(&mut server);
    prepare_share(&mut a2.store, &a2.account, other, &info(&carol, "carol"), Permission::Read, 1).unwrap();
    verify_contact(&mut a2.store, carol.user_id).unwrap();

    a1.sync(&mut server);
    a2.sync(&mut server);
    a1.sync(&mut server);
    let (v1, v2) = (a1.store.vault().unwrap().unwrap(), a2.store.vault().unwrap().unwrap());
    assert!(v1.local.is_none() && v2.local.is_none());
    assert_eq!(v1.base, v2.base);
    assert_eq!(v1.base.settings["theme"], "dark");
    assert_eq!(v1.base.contacts.keys().copied().collect::<BTreeSet<_>>(), BTreeSet::from([bob.user_id, carol.user_id]));
    assert!(v1.base.contacts[&carol.user_id].verified);
}

#[test]
fn rollback_is_ignored() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("v1");
    a1.sync(&mut server);
    let old = server.memos[&id].clone();
    a1.edit(id, "v2");
    a1.sync(&mut server);
    a2.sync(&mut server);

    // Same epoch, old ciphertext: a server replaying version 1.
    server.memos.insert(id, old);
    server.fan_out(Entity::Memo(id), [alice.user_id]);
    assert_eq!(a2.sync(&mut server), vec![SyncEvent::Rollback { memo_id: id, version: 1, highest_seen: 2 }]);
    assert_eq!(a2.body(id), "v2");
}

#[test]
fn editor_helpers() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let id = a1.create("- [ ] milk\n- [ ] eggs");
    assert!(has_unsynced(&a1.store).unwrap());
    a1.sync(&mut server);
    assert!(!has_unsynced(&a1.store).unwrap());

    edit_body(&mut a1.store, id, "- [ ] milk\n- [ ] eggs", 5, true).unwrap();
    assert!(clean(&a1, id), "an unchanged body is not an edit");
    assert!(toggle_task(&mut a1.store, id, 2, 5).unwrap());
    assert_eq!(a1.body(id), "- [x] milk\n- [ ] eggs");
    assert_eq!(a1.memo(id).local.unwrap().edited_at, 5);
    assert!(!toggle_task(&mut a1.store, id, 0, 5).unwrap());
    assert!(has_unsynced(&a1.store).unwrap());

    let new = NewAttachment { mime: "image/png".into(), width: 1, height: 1, bytes: b"png", thumbnail: None };
    let attachment = add_attachment(&mut a1.store, id, new).unwrap();
    let with_image = format!("{}\n![](attachment:{})", a1.body(id), attachment.id);
    edit_body(&mut a1.store, id, &with_image, 6, true).unwrap();
    assert_eq!(memo_attachment(&a1.store, id, attachment.id, AttachmentKind::Original).unwrap().unwrap(), b"png");
    a1.sync(&mut server);
    a2.sync(&mut server);

    // Both sides edit the first line; a2 resolves with a body that keeps the image.
    edit_body(&mut a1.store, id, &with_image.replace("milk", "oat milk"), 7, false).unwrap();
    edit_body(&mut a2.store, id, &with_image.replace("milk", "soy milk"), 7, false).unwrap();
    a1.sync(&mut server);
    assert_eq!(a2.sync(&mut server), vec![SyncEvent::Conflict { memo_id: id }]);
    let resolved = with_image.replace("milk", "oat and soy milk");
    resolve_with_body(&mut a2.store, id, &resolved, 8).unwrap();
    assert_eq!(a2.memo(id).current().unwrap().attachments, vec![attachment.clone()]);
    a2.sync(&mut server);
    a1.sync(&mut server);
    assert_eq!(a1.body(id), resolved);

    // Taking their side exactly leaves nothing to push.
    edit_body(&mut a1.store, id, "mine", 9, true).unwrap();
    edit_body(&mut a2.store, id, "theirs", 9, true).unwrap();
    assert!(a2.memo(id).current().unwrap().attachments.is_empty(), "prune drops the image");
    a2.sync(&mut server);
    a1.sync(&mut server);
    resolve_with_body(&mut a1.store, id, "theirs", 10).unwrap();
    assert!(clean(&a1, id));
    assert_eq!(memo_attachment(&a1.store, id, attachment.id, AttachmentKind::Original), Err(crate::Error::NotFound));
}

#[test]
fn filters_activity_stats_links_and_saved_filters() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let (mut a1, mut a2) = (Device::new(&alice), Device::new(&alice));
    let create = |d: &mut Device, body: &str, now: i64| {
        create_memo(&mut d.store, &d.account, MemoPayload::new(body, now), now).unwrap()
    };
    let target = create(&mut a1, "# Target #work", 1000);
    let tasks = create(&mut a1, &format!("Plan\n- [ ] open\n- [x] done\nsee [target](memo:{target})"), 2000);
    let code = create(&mut a1, "```rust\nfn x() {}\n```", 3000);
    let archived = create(&mut a1, "old [t](memo:{target}) https://x.org", 500);
    set_memo_state(&mut a1.store, archived, false, true).unwrap();
    let list = |d: &Device, filter: MemoFilter| -> Vec<Uuid> {
        memo_list(&d.store, &filter).unwrap().into_iter().map(|m| m.id).collect()
    };

    assert_eq!(list(&a1, MemoFilter { has: vec![Has::Tasks], ..Default::default() }), [tasks]);
    assert_eq!(list(&a1, MemoFilter { has: vec![Has::OpenTasks, Has::Links], ..Default::default() }), [tasks]);
    assert_eq!(list(&a1, MemoFilter { has: vec![Has::Code], ..Default::default() }), [code]);
    assert!(list(&a1, MemoFilter { has: vec![Has::Images], ..Default::default() }).is_empty());
    let day = MemoFilter { created_from: Some(1000), created_before: Some(3000), ..Default::default() };
    assert_eq!(list(&a1, day).into_iter().collect::<BTreeSet<_>>(), BTreeSet::from([target, tasks]));

    assert_eq!(activity(&a1.store).unwrap(), [1000, 2000, 3000]);
    let s = stats(&a1.store).unwrap();
    assert_eq!((s.memos, s.archived, s.tags, s.open_tasks, s.done_tasks, s.since), (3, 1, 1, 1, 1, Some(1000)));

    // Backlinks come from every memo, archived ones included; the archived one has no valid link.
    let detail = memo_detail(&a1.store, alice.user_id, target).unwrap();
    assert_eq!(detail.linked_from, [MemoLink { id: tasks, title: "Plan".into() }]);

    // Saved filters live in the vault and sync; two devices can add one each.
    a1.sync(&mut server);
    let work = MemoFilter { tag: Some("work".into()), has: vec![Has::OpenTasks], ..Default::default() };
    let first = save_filter(&mut a1.store, " Open work ", &work).unwrap();
    a1.sync(&mut server);
    a2.sync(&mut server);
    let second =
        save_filter(&mut a2.store, "Code", &MemoFilter { has: vec![Has::Code], ..Default::default() }).unwrap();
    let third = save_filter(&mut a1.store, "All", &MemoFilter::default()).unwrap();
    a1.sync(&mut server);
    a2.sync(&mut server);
    a1.sync(&mut server);
    let names = |d: &Device| saved_filters(&d.store).unwrap().into_iter().map(|f| (f.id, f.name)).collect::<Vec<_>>();
    let expected = vec![(third, "All".to_owned()), (second, "Code".into()), (first, "Open work".into())];
    assert_eq!(names(&a1), expected);
    assert_eq!(names(&a2), expected);
    let saved = &saved_filters(&a1.store).unwrap()[2];
    assert_eq!(
        (saved.tag.as_deref(), saved.has.as_slice(), saved.query.as_str()),
        (Some("work"), &[Has::OpenTasks][..], "")
    );
    delete_filter(&mut a2.store, first).unwrap();
    a2.sync(&mut server);
    a1.sync(&mut server);
    assert_eq!(names(&a1).len(), 2);
}

fn revoke(server: &mut Server, owner: &mut Device, id: Uuid, users: &[Uuid]) {
    let prepared = prepare_revoke(&owner.store, &owner.account, id, users).unwrap();
    let response = server.revoke(owner.account.user_id, id, &prepared.request);
    apply_revoke(&mut owner.store, prepared, response).unwrap();
}

fn shares(device: &Device) -> BTreeMap<Uuid, BTreeSet<Uuid>> {
    device.store.vault().unwrap().unwrap().current().shares.clone()
}

#[test]
fn revoke_only_wraps_for_members_the_owner_shared_with() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    // The server's own account. Alice never shared anything with it.
    let mallory = user(&mut server, "mallory");
    let mut a = Device::new(&alice);
    let id = a.create("secret plans");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Read);
    a.sync(&mut server);
    assert_eq!(shares(&a), BTreeMap::from([(id, BTreeSet::from([bob.user_id]))]));

    // The server lists mallory as a member and tells alice.
    server.members.insert((id, mallory.user_id), Permission::Read);
    server.fan_out(Entity::Access(id), [alice.user_id]);
    a.sync(&mut server);
    let unconfirmed = |device: &Device| -> Vec<Uuid> {
        let detail = memo_detail(&device.store, alice.user_id, id).unwrap();
        detail.members.iter().filter(|m| m.unconfirmed).map(|m| m.user_id).collect()
    };
    assert_eq!(unconfirmed(&a), vec![mallory.user_id]);
    assert_eq!(
        prepare_revoke(&a.store, &a.account, id, &[bob.user_id]).unwrap_err(),
        crate::Error::UnknownMember(mallory.user_id)
    );

    // Another device of alice knows from the vault who she shared with.
    let mut a2 = Device::new(&alice);
    a2.sync(&mut server);
    assert_eq!(unconfirmed(&a2), vec![mallory.user_id]);
    assert!(prepare_revoke(&a2.store, &a2.account, id, &[bob.user_id]).is_err());

    // Removing the unknown member works, and they get no key.
    let prepared = prepare_revoke(&a.store, &a.account, id, &[mallory.user_id]).unwrap();
    let wrapped_for: BTreeSet<Uuid> = prepared.request.keys.iter().map(|k| k.user_id).collect();
    assert_eq!(wrapped_for, BTreeSet::from([alice.user_id, bob.user_id]));
    let response = server.revoke(alice.user_id, id, &prepared.request);
    apply_revoke(&mut a.store, prepared, response).unwrap();
    a.sync(&mut server);
    assert!(unconfirmed(&a).is_empty());

    // Removing bob drops the record, on every device.
    revoke(&mut server, &mut a, id, &[bob.user_id]);
    a.sync(&mut server);
    a2.sync(&mut server);
    assert!(shares(&a).is_empty() && shares(&a2).is_empty());
    assert_eq!(a2.memo(id).key_version, 3);
}

#[test]
fn a_pinned_name_under_another_user_id_is_refused() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let mut a = Device::new(&alice);
    let first = a.create("first");
    a.sync(&mut server);
    share(&mut server, &mut a, first, &bob, Permission::Read);
    verify_contact(&mut a.store, bob.user_id).unwrap();
    a.sync(&mut server);
    let real = UserInfo { user_id: bob.user_id, username: "bob".into(), public_key: *bob.keypair.public_key() };
    assert_eq!(contact_trust(&a.store, alice.user_id, &real).unwrap().0, Trust::Verified);

    // The server answers a lookup of "bob" with another account.
    let second = a.create("for bob only");
    a.sync(&mut server);
    let other = user(&mut server, "bob");
    let fake = UserInfo { user_id: other.user_id, username: "Bob".into(), public_key: *other.keypair.public_key() };
    assert_eq!(contact_trust(&a.store, alice.user_id, &fake).unwrap().0, Trust::Changed);
    assert_eq!(
        prepare_share(&mut a.store, &a.account, second, &fake, Permission::Read, 9).unwrap_err(),
        crate::Error::KeyChanged(other.user_id)
    );
    let vault = a.store.vault().unwrap().unwrap();
    assert!(!vault.current().contacts.contains_key(&other.user_id) && !vault.current().shares.contains_key(&second));

    // The same account as the owner of a shared memo: the alarm, no pin, and its keys are not accepted.
    let mut o = Device::new(&other);
    let theirs = o.create("from bob");
    o.sync(&mut server);
    share(&mut server, &mut o, theirs, &alice, Permission::Read);
    let events = a.sync(&mut server);
    assert!(events.contains(&SyncEvent::KeyChanged { user_id: other.user_id }), "{events:?}");
    assert!(events.contains(&SyncEvent::KeyRejected { memo_id: theirs, owner_id: other.user_id }), "{events:?}");
    assert!(a.memo(theirs).base.is_none());
    assert!(!a.store.vault().unwrap().unwrap().current().contacts.contains_key(&other.user_id));

    // A pinned user is shown under the name they were pinned with, whatever the server calls them.
    server.users.get_mut(&bob.user_id).unwrap().0 = "alice2".into();
    server.fan_out(Entity::Access(first), [alice.user_id]);
    a.sync(&mut server);
    let detail = memo_detail(&a.store, alice.user_id, first).unwrap();
    let member = detail.members.iter().find(|m| m.user_id == bob.user_id).unwrap();
    assert_eq!((member.username.as_str(), member.trust), ("bob", Trust::Verified));
}

#[test]
fn restore_recreates_under_the_newest_key() {
    use crate::content::{MemoContext, open_memo};
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let (mut a, mut b) = (Device::new(&alice), Device::new(&bob));
    a.sync(&mut server);
    let backup = server.clone();
    let id = a.create("v1");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Read);
    a.sync(&mut server);
    b.sync(&mut server);
    let bobs_key = b.store.access(id).unwrap().unwrap().keys[&1].clone();
    revoke(&mut server, &mut a, id, &[bob.user_id]);
    a.sync(&mut server);
    a.edit(id, "written after bob was removed");
    a.sync(&mut server);
    let newest = a.store.access(id).unwrap().unwrap().keys[&2].clone();

    // The server says it was restored (new epoch) and does not return the memo.
    server = Server { epoch: "epoch-2".into(), ..backup };
    a.sync(&mut server);
    let memo = &server.memos[&id];
    assert_eq!((memo.version, memo.key_version), (1, 1));
    let ctx = MemoContext { memo_id: id, version: 1, key_version: 1 };
    let ciphertext = memo.ciphertext.as_ref().unwrap();
    assert_eq!(open_memo(&bobs_key, &ctx, ciphertext), Err(crate::Error::Decrypt));
    assert_eq!(open_memo(&newest, &ctx, ciphertext).unwrap().body, "written after bob was removed");
    assert!(clean(&a, id) && shares(&a).is_empty());
}

#[test]
fn the_owner_is_the_one_the_keys_verify() {
    let mut server = Server::new();
    let (alice, mallory) = (user(&mut server, "alice"), user(&mut server, "mallory"));
    let (mut a, mut m) = (Device::new(&alice), Device::new(&mallory));
    let id = m.create("mallory's memo");
    m.sync(&mut server);
    share(&mut server, &mut m, id, &alice, Permission::Write);

    // The server presents the memo as alice's own.
    let mut session = SyncSession::start(&mut a.store, &a.account, 1000).unwrap();
    loop {
        let mut response = server.sync(alice.user_id, session.request());
        for change in &mut response.changes {
            if let Change::Memo(memo) = change {
                memo.owner_id = alice.user_id;
            }
        }
        if session.handle(&mut a.store, &a.account, response).unwrap() == Step::Done {
            break;
        }
    }
    assert_eq!(a.body(id), "mallory's memo");
    let detail = memo_detail(&a.store, alice.user_id, id).unwrap();
    assert!(!detail.own && detail.summary.shared);
    assert_eq!((detail.summary.owner_id, a.memo(id).owner_id), (mallory.user_id, mallory.user_id));

    // It also hides mallory from the member list: still someone else's memo, and shared.
    let mut access = a.store.access(id).unwrap().unwrap();
    access.members.retain(|m| m.user_id == alice.user_id);
    a.store.put_access(&access).unwrap();
    let detail = memo_detail(&a.store, alice.user_id, id).unwrap();
    assert!(!detail.own && detail.summary.shared);
    assert_eq!(delete_memo(&mut a.store, &a.account, id), Err(crate::Error::NotOwner));
    let info =
        UserInfo { user_id: mallory.user_id, username: "mallory".into(), public_key: *mallory.keypair.public_key() };
    assert_eq!(
        prepare_share(&mut a.store, &a.account, id, &info, Permission::Read, 1).unwrap_err(),
        crate::Error::NotOwner
    );
}

#[test]
fn access_without_the_vault_is_refused() {
    let mut server = Server::new();
    let (alice, bob) = (user(&mut server, "alice"), user(&mut server, "bob"));
    let (mut a, mut b) = (Device::new(&alice), Device::new(&bob));
    let id = a.create("shared");
    a.sync(&mut server);
    share(&mut server, &mut a, id, &bob, Permission::Read);

    // The server keeps the vault from a new device: nothing is pinned, nothing is applied.
    let mut session = SyncSession::start(&mut b.store, &b.account, 1000).unwrap();
    let mut response = server.sync(bob.user_id, session.request());
    response.changes.retain(|c| !matches!(c, Change::Vault(_)));
    assert_eq!(
        session.handle(&mut b.store, &b.account, response),
        Err(crate::Error::Malformed("sync response without the vault"))
    );
    assert!(b.store.vault().unwrap().is_none() && b.store.memo(id).unwrap().is_none());
    assert!(b.sync(&mut server).is_empty());
    assert_eq!(b.body(id), "shared");
}

#[test]
fn views_follow_the_store() {
    let mut server = Server::new();
    let alice = user(&mut server, "alice");
    let mut a = Device::new(&alice);
    let mut views = Views::new();
    let first = a.create("# First\n#work see [second](memo:00000000-0000-7000-8000-000000000000)");
    assert_eq!(views.memo_list(&a.store, &MemoFilter::default()).unwrap().len(), 1);
    let second = a.create("# Second\n- [ ] task #work #home");
    a.edit(first, &format!("# First\n#work see [second](memo:{second})"));
    let list = views.memo_list(&a.store, &MemoFilter::default()).unwrap();
    assert_eq!(list.len(), 2);
    let tags = views.tag_counts(&a.store).unwrap();
    assert_eq!(tags.iter().map(|t| (t.tag.as_str(), t.count)).collect::<Vec<_>>(), vec![("work", 2), ("home", 1)]);
    let open = MemoFilter { has: vec![Has::OpenTasks], query: "SECOND".into(), ..Default::default() };
    assert_eq!(views.memo_list(&a.store, &open).unwrap().iter().map(|s| s.id).collect::<Vec<_>>(), vec![second]);
    assert_eq!(views.stats(&a.store).unwrap().open_tasks, 1);
    let detail = views.memo_detail(&a.store, alice.user_id, second).unwrap();
    assert_eq!(detail.linked_from.iter().map(|l| (l.id, l.title.as_str())).collect::<Vec<_>>(), vec![(first, "First")]);

    set_memo_state(&mut a.store, second, false, true).unwrap();
    assert_eq!(views.memo_list(&a.store, &MemoFilter::default()).unwrap().len(), 1);
    assert_eq!(views.activity(&a.store).unwrap().len(), 1);
    delete_memo(&mut a.store, &a.account, first).unwrap();
    assert!(views.memo_list(&a.store, &MemoFilter::default()).unwrap().is_empty());
    // The same answers as without a cache.
    assert_eq!(views.stats(&a.store).unwrap(), stats(&a.store).unwrap());
}

#[test]
fn memory_store_evicts_the_least_recently_used_blobs() {
    let mut store = MemoryStore::new();
    let ids: Vec<Uuid> = (1..=3).map(|i| Uuid::from_bytes([i; 16])).collect();
    for id in &ids {
        store.put_blob(*id, AttachmentKind::Original, &[0; 100]).unwrap();
    }
    store
        .put_attachment(&AttachmentRecord {
            id: ids[0],
            memo_id: Uuid::nil(),
            on_server: false,
            uploaded: false,
            thumb_uploaded: false,
            upload_original: true,
            upload_thumbnail: false,
            pending_delete: false,
        })
        .unwrap();
    // A transaction's snapshot shares the blobs and rolls back to them.
    store.begin().unwrap();
    store.delete_attachment(ids[1]).unwrap();
    store.rollback();
    assert!(store.blob(ids[1], AttachmentKind::Original).unwrap().is_some());

    assert_eq!(store.trim_blobs(100), 100);
    let cached = |store: &MemoryStore, id| store.blob(id, AttachmentKind::Original).unwrap().is_some();
    assert!(cached(&store, ids[0]), "waiting to be uploaded");
    assert!(cached(&store, ids[1]), "read last");
    assert!(!cached(&store, ids[2]));
}
