//! `memo-core` for the native clients: the Android app (over JNI, `crates/android`) and the
//! desktop app (over Tauri IPC, `desktop/`). An app holds one account at a time, so the state
//! (pending account flows, the unlocked account, its SQLite store and the running sync session) is
//! a single global. The app does the HTTP and the key storage and passes bodies through.
//!
//! Every call is [`call`]: a JSON request `{"call": "<name>", ...}` (see [`Request`]) plus optional
//! bytes. The reply is one status byte (0 ok, 1 error), a big-endian `u32` length, that much JSON,
//! then optional bytes. Errors are `{"code", "message"}`; codes are [`memo_core::Error::code`] or
//! the bridge's own (`bad_request`, `locked`, `no_session`, `panic`, ...).
//!
//! Calls block: Argon2 takes about a second, so apps never call from their UI thread.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::Mutex,
};

use memo_core::{
    AccountKey, AttachmentKind, KdfParams, MemoPayload, PasswordKeys, RecoveryKey, Salt, UnlockedAccount,
    api::{
        DeleteAccountRequest, DeviceInfo, KdfConfig, LoginRequest, LoginResponse, NewPassword, PasswordChangeRequest,
        Permission, PreloginResponse, RecoverCompleteRequest, RecoverRequest, RecoverResponse, RegisterRequest,
        Revision, RevokeResponse, SyncResponse, UserInfo,
    },
    b64_decode, b64_encode, image, render,
    sync::{self, MemoFilter, NewAttachment, NewThumbnail, PreparedRevoke, Step, Store, SyncSession, Views},
};
use memo_sqlite::SqliteStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, PartialEq, Eq)]
pub struct Fail {
    pub code: String,
    pub message: String,
}

impl Fail {
    fn new(code: &str, message: impl std::fmt::Display) -> Self {
        Self { code: code.into(), message: message.to_string() }
    }
}

impl From<memo_core::Error> for Fail {
    fn from(e: memo_core::Error) -> Self {
        Self::new(e.code(), &e)
    }
}

type Result<T, E = Fail> = std::result::Result<T, E>;

/// A reply: JSON and, for calls that return bytes, the bytes.
#[derive(Debug, Default)]
pub struct Reply {
    pub json: Value,
    pub data: Option<Vec<u8>>,
}

fn reply(value: impl Serialize) -> Result<Reply> {
    Ok(Reply { json: serde_json::to_value(value).expect("serializable"), data: None })
}

fn bytes(data: Option<Vec<u8>>) -> Result<Reply> {
    Ok(Reply { json: json!(data.is_some()), data })
}

/// The keys a device keeps in the OS keystore to unlock without the password
/// (`docs/crypto.md#account-flows`). The device token is Kotlin's.
#[derive(Serialize, Deserialize)]
pub struct Secrets {
    user_id: Uuid,
    account_key: String,
    private_key: String,
}

#[derive(Deserialize)]
pub struct ThumbMeta {
    mime: String,
    width: u32,
    height: u32,
}

#[derive(Deserialize)]
#[serde(tag = "call", rename_all = "snake_case")]
pub enum Request {
    /// Once per process: where the databases live.
    Init {
        dir: PathBuf,
    },

    PasswordProblem {
        password: String,
    },
    /// `memo_core::render::Block[]`.
    Render {
        body: String,
    },
    /// Safe HTML (`memo_core::render::to_html`), for web views.
    RenderHtml {
        body: String,
    },
    /// Data: an image. Returns `{mime, width, height, orientation}` and the cleaned bytes.
    CleanImage,
    /// Data: decrypted bytes. Whether they may be shown inline as `mime` (`docs/crypto.md#attachments`).
    Renderable {
        mime: String,
    },

    /// Slow. Returns `{request, recovery_key}`; send the request to `POST /auth/register`.
    RegisterStart {
        password: String,
        username: String,
        invite: String,
        device: DeviceInfo,
    },
    /// After the `201`. Opens the account; returns [`Opened`].
    RegisterFinish,
    /// Slow. Returns the body of `POST /auth/login`.
    LoginStart {
        password: String,
        prelogin: PreloginResponse,
        username: String,
        device: DeviceInfo,
    },
    LoginFinish {
        response: LoginResponse,
    },
    /// Returns the body of `POST /auth/recover`.
    RecoverStart {
        recovery_key: String,
        username: String,
    },
    /// Slow. Returns `{request, recovery_key}` for `POST /auth/recover/complete`.
    RecoverComplete {
        response: RecoverResponse,
        new_password: String,
        device: DeviceInfo,
    },
    RecoverFinish,
    /// Unlocks with the keys from the keystore.
    Open {
        secrets: Secrets,
    },
    /// Forgets the keys in memory; the database stays.
    Close,
    /// Closes and deletes the account's database (logout).
    Forget {
        user_id: Uuid,
    },

    SyncStart {
        now: i64,
    },
    /// Returns the next request body, or `null` when the session is done.
    SyncHandle {
        response: SyncResponse,
    },
    /// Returns the session's events.
    SyncFinish,
    PendingUploads,
    Blob {
        attachment_id: Uuid,
        kind: AttachmentKind,
    },
    UploadDone {
        attachment_id: Uuid,
        kind: AttachmentKind,
    },
    /// Data: a downloaded envelope.
    PutBlob {
        attachment_id: Uuid,
        kind: AttachmentKind,
    },
    /// Returns the bytes freed.
    TrimBlobs {
        budget: u64,
    },
    HasUnsynced,

    MemoList {
        filter: MemoFilter,
    },
    TagCounts,
    /// Creation times of the memos in the main list, for the calendar.
    Activity,
    Stats,
    SavedFilters,
    /// Saves the query, tag and quick filters of `filter`; returns the new filter's id.
    SaveFilter {
        name: String,
        filter: MemoFilter,
    },
    DeleteFilter {
        filter_id: Uuid,
    },
    MemoDetail {
        memo_id: Uuid,
    },
    /// Decrypted bytes, or `false` and no bytes if the blob is not cached yet.
    Attachment {
        memo_id: Uuid,
        attachment_id: Uuid,
        kind: AttachmentKind,
    },

    Create {
        body: String,
        now: i64,
    },
    Edit {
        memo_id: Uuid,
        body: String,
        now: i64,
        prune: bool,
    },
    ToggleTask {
        memo_id: Uuid,
        offset: usize,
        now: i64,
    },
    Delete {
        memo_id: Uuid,
    },
    SetState {
        memo_id: Uuid,
        pinned: bool,
        archived: bool,
    },
    Resolve {
        memo_id: Uuid,
        body: String,
        now: i64,
    },
    SaveAsCopy {
        memo_id: Uuid,
        now: i64,
    },
    /// Data: the cleaned original, then the thumbnail if `thumb` is set. Returns the attachment id.
    AddAttachment {
        memo_id: Uuid,
        mime: String,
        width: u32,
        height: u32,
        size: usize,
        thumb: Option<ThumbMeta>,
    },

    OpenRevision {
        memo_id: Uuid,
        revision: Revision,
    },
    RestoreRevision {
        memo_id: Uuid,
        revision: Revision,
        now: i64,
    },

    ContactTrust {
        user: UserInfo,
    },
    PrepareShare {
        memo_id: Uuid,
        user: UserInfo,
        permission: Permission,
        now: i64,
    },
    PrepareRevoke {
        memo_id: Uuid,
        user_ids: Vec<Uuid>,
    },
    ApplyRevoke {
        memo_id: Uuid,
        response: RevokeResponse,
    },
    VerifyContact {
        user_id: Uuid,
    },

    /// Slow (Argon2 twice). Returns the body of `POST /auth/password`.
    PasswordChange {
        current_password: String,
        prelogin: PreloginResponse,
        new_password: String,
        revoke_other_devices: bool,
    },
    /// Slow. Returns the body of `DELETE /me`.
    DeleteAccount {
        password: String,
        prelogin: PreloginResponse,
    },
}

/// What opening an account returns: who it is, and the keys to put in the keystore.
#[derive(Serialize)]
struct Opened {
    user_id: Uuid,
    fingerprint: String,
    secrets: Secrets,
}

enum Pending {
    Registration(Box<memo_core::Registration>),
    Login(PasswordKeys),
    Recovery(RecoveryKey),
    Recovered(UnlockedAccount),
}

struct Client {
    account: UnlockedAccount,
    store: SqliteStore,
    /// The list views, kept between calls until the store changes.
    views: Views,
    session: Option<SyncSession>,
    revokes: Vec<PreparedRevoke>,
}

#[derive(Default)]
struct State {
    dir: Option<PathBuf>,
    pending: Option<Pending>,
    client: Option<Client>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Handles one encoded request (see the crate docs).
pub fn call(request: &[u8], data: Option<&[u8]>) -> Vec<u8> {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let request: Request = serde_json::from_slice(request).map_err(|e| Fail::new("bad_request", e))?;
        // A panic in an earlier call poisons the lock; the state is still consistent at call
        // boundaries (the store rolls back its own transactions).
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        handle(state.get_or_insert_with(State::default), request, data)
    }));
    let result = result.unwrap_or_else(|panic| {
        let message =
            panic.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| panic.downcast_ref::<String>().cloned());
        Err(Fail::new("panic", message.unwrap_or_else(|| "panic".into())))
    });
    encode(result)
}

fn encode(result: Result<Reply>) -> Vec<u8> {
    let (status, json, data) = match result {
        Ok(reply) => (0u8, reply.json, reply.data),
        Err(fail) => (1u8, json!({ "code": fail.code, "message": fail.message }), None),
    };
    let json = serde_json::to_vec(&json).expect("serializable");
    let mut out = Vec::with_capacity(5 + json.len() + data.as_ref().map_or(0, Vec::len));
    out.push(status);
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(data.as_deref().unwrap_or_default());
    out
}

fn handle(state: &mut State, request: Request, data: Option<&[u8]>) -> Result<Reply> {
    use Request::*;
    match request {
        Init { dir } => {
            create_private_dir(&dir).map_err(|e| Fail::new("store", e))?;
            state.dir = Some(dir);
            reply(())
        }
        PasswordProblem { password } => reply(memo_core::check_password_policy(&password).err().map(|e| e.to_string())),
        Render { body } => reply(render::to_blocks(&body)),
        RenderHtml { body } => reply(render::to_html(&body)),
        Renderable { mime } => reply(image::renderable_as(data.unwrap_or_default(), &mime)),
        CleanImage => {
            let clean = image::clean_image(data.unwrap_or_default())?;
            let meta = json!({
                "mime": clean.kind.mime(),
                "width": clean.width,
                "height": clean.height,
                "orientation": clean.orientation,
            });
            Ok(Reply { json: meta, data: Some(clean.bytes) })
        }

        RegisterStart { password, username, invite, device } => {
            let r = memo_core::register(&password)?;
            let request = RegisterRequest {
                invite_code: invite.trim().into(),
                user_id: r.user_id,
                username: username.trim().into(),
                kdf: kdf_config(r.kdf_salt, r.kdf),
                auth_key: r.auth_key.clone(),
                recovery_auth_key: r.recovery_auth_key.clone(),
                public_key: r.public_key,
                enc_account_key: r.enc_account_key.clone(),
                enc_account_key_recovery: r.enc_account_key_recovery.clone(),
                enc_private_key: r.enc_private_key.clone(),
                vault: r.vault.clone(),
                device,
            };
            let recovery_key = r.recovery_key.to_display().to_string();
            state.pending = Some(Pending::Registration(Box::new(r)));
            reply(json!({ "request": request, "recovery_key": recovery_key }))
        }
        RegisterFinish => match state.pending.take() {
            Some(Pending::Registration(r)) => open(state, r.account),
            _ => Err(Fail::new("no_pending", "no registration in progress")),
        },
        LoginStart { password, prelogin, username, device } => {
            let keys = password_keys(&password, &prelogin)?;
            let request = LoginRequest { username: username.trim().into(), auth_key: keys.auth_key.clone(), device };
            state.pending = Some(Pending::Login(keys));
            reply(request)
        }
        LoginFinish { response: r } => match state.pending.take() {
            Some(Pending::Login(keys)) => {
                let account = memo_core::unlock(
                    r.user_id,
                    &keys.master_key,
                    &r.public_key,
                    &r.enc_account_key,
                    &r.enc_private_key,
                )?;
                open(state, account)
            }
            _ => Err(Fail::new("no_pending", "no login in progress")),
        },
        RecoverStart { recovery_key, username } => {
            let key = RecoveryKey::parse(&recovery_key)?;
            let request = RecoverRequest { username: username.trim().into(), recovery_auth_key: key.auth_key() };
            state.pending = Some(Pending::Recovery(key));
            reply(request)
        }
        RecoverComplete { response: r, new_password, device } => {
            let Some(Pending::Recovery(key)) = &state.pending else {
                return Err(Fail::new("no_pending", "no recovery in progress"));
            };
            let account_key = memo_core::recover_account_key(r.user_id, key, &r.enc_account_key_recovery)?;
            let account =
                memo_core::unlock_with_account_key(r.user_id, account_key, &r.public_key, &r.enc_private_key)?;
            let password = account.change_password(&new_password)?;
            let recovery = account.new_recovery_key();
            let request = RecoverCompleteRequest {
                recovery_token: r.recovery_token,
                kdf: kdf_config(password.kdf_salt, password.kdf),
                auth_key: password.auth_key,
                enc_account_key: password.enc_account_key,
                recovery_auth_key: recovery.recovery_auth_key,
                enc_account_key_recovery: recovery.enc_account_key_recovery,
                device,
            };
            state.pending = Some(Pending::Recovered(account));
            reply(json!({ "request": request, "recovery_key": recovery.recovery_key.to_display().to_string() }))
        }
        RecoverFinish => match state.pending.take() {
            Some(Pending::Recovered(account)) => open(state, account),
            _ => Err(Fail::new("no_pending", "no recovery in progress")),
        },
        Open { secrets } => {
            let account_key = AccountKey::from_bytes(&b64_decode(&secrets.account_key)?)?;
            let private_key = b64_decode(&secrets.private_key)?;
            let account = UnlockedAccount::from_parts(secrets.user_id, account_key, &private_key)?;
            open(state, account)
        }
        Close => {
            state.client = None;
            state.pending = None;
            reply(())
        }
        Forget { user_id } => {
            state.client = None;
            state.pending = None;
            let path = db_path(state, user_id)?;
            for suffix in ["", "-wal", "-shm"] {
                let mut file = path.clone().into_os_string();
                file.push(suffix);
                match std::fs::remove_file(&file) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(Fail::new("store", e)),
                }
            }
            reply(())
        }

        PasswordChange { current_password, prelogin, new_password, revoke_other_devices } => {
            let c = client(state)?;
            let current = password_keys(&current_password, &prelogin)?;
            let new = c.account.change_password(&new_password)?;
            reply(PasswordChangeRequest {
                auth_key: current.auth_key,
                new: NewPassword {
                    kdf: kdf_config(new.kdf_salt, new.kdf),
                    auth_key: new.auth_key,
                    enc_account_key: new.enc_account_key,
                },
                revoke_other_devices,
            })
        }
        DeleteAccount { password, prelogin } => {
            client(state)?;
            reply(DeleteAccountRequest { auth_key: password_keys(&password, &prelogin)?.auth_key })
        }

        request => client_call(client(state)?, request, data),
    }
}

fn client_call(c: &mut Client, request: Request, data: Option<&[u8]>) -> Result<Reply> {
    use Request::*;
    let (store, account) = (&mut c.store, &c.account);
    match request {
        SyncStart { now } => {
            let session = SyncSession::start(store, account, now)?;
            let request = serde_json::to_value(session.request()).expect("serializable");
            c.session = Some(session);
            reply(request)
        }
        SyncHandle { response } => {
            let session = c.session.as_mut().ok_or_else(|| Fail::new("no_session", "no sync in progress"))?;
            match session.handle(store, account, response)? {
                Step::Send => reply(session.request()),
                Step::Done => reply(()),
            }
        }
        SyncFinish => reply(c.session.take().map(|s| s.into_report().events).unwrap_or_default()),
        PendingUploads => {
            let uploads: Vec<Value> = sync::pending_uploads(store)?
                .into_iter()
                .map(|u| json!({ "memo_id": u.memo_id, "attachment_id": u.attachment_id, "kind": u.kind }))
                .collect();
            reply(uploads)
        }
        Blob { attachment_id, kind } => bytes(store.blob(attachment_id, kind)?),
        UploadDone { attachment_id, kind } => reply(sync::upload_done(store, attachment_id, kind)?),
        PutBlob { attachment_id, kind } => {
            let envelope = data.ok_or_else(|| Fail::new("bad_request", "no data"))?;
            reply(store.put_blob(attachment_id, kind, envelope)?)
        }
        TrimBlobs { budget } => reply(store.trim_blobs(budget)?),
        HasUnsynced => reply(sync::has_unsynced(store)?),

        MemoList { filter } => reply(c.views.memo_list(store, &filter)?),
        TagCounts => reply(c.views.tag_counts(store)?),
        Activity => reply(c.views.activity(store)?),
        Stats => reply(c.views.stats(store)?),
        SavedFilters => reply(sync::saved_filters(store)?),
        SaveFilter { name, filter } => reply(sync::save_filter(store, &name, &filter)?),
        DeleteFilter { filter_id } => reply(sync::delete_filter(store, filter_id)?),
        MemoDetail { memo_id } => reply(c.views.memo_detail(store, account.user_id, memo_id)?),
        Attachment { memo_id, attachment_id, kind } => {
            bytes(sync::memo_attachment(store, memo_id, attachment_id, kind)?)
        }

        Create { body, now } => reply(sync::create_memo(store, account, MemoPayload::new(body, now), now)?),
        Edit { memo_id, body, now, prune } => reply(sync::edit_body(store, memo_id, &body, now, prune)?),
        ToggleTask { memo_id, offset, now } => reply(sync::toggle_task(store, memo_id, offset, now)?),
        Delete { memo_id } => reply(sync::delete_memo(store, account, memo_id)?),
        SetState { memo_id, pinned, archived } => reply(sync::set_memo_state(store, memo_id, pinned, archived)?),
        Resolve { memo_id, body, now } => reply(sync::resolve_with_body(store, memo_id, &body, now)?),
        SaveAsCopy { memo_id, now } => reply(sync::save_as_copy(store, account, memo_id, now)?),
        AddAttachment { memo_id, mime, width, height, size, thumb } => {
            let data = data.unwrap_or_default();
            if size > data.len() || (thumb.is_none() && size != data.len()) {
                return Err(Fail::new("bad_request", "data does not match the sizes"));
            }
            let (original, thumb_bytes) = data.split_at(size);
            if !image::renderable_as(original, &mime) {
                return Err(Fail::new("bad_image", "the bytes are not the stated image type"));
            }
            let thumbnail = match thumb {
                Some(t) if image::renderable_as(thumb_bytes, &t.mime) => {
                    Some(NewThumbnail { mime: t.mime, width: t.width, height: t.height, bytes: thumb_bytes })
                }
                Some(_) => return Err(Fail::new("bad_image", "the thumbnail is not the stated image type")),
                None => None,
            };
            let new = NewAttachment { mime, width, height, bytes: original, thumbnail };
            reply(sync::add_attachment(store, memo_id, new)?.id)
        }

        OpenRevision { memo_id, revision } => reply(sync::open_revision(store, memo_id, &revision)?.body),
        RestoreRevision { memo_id, revision, now } => reply(sync::restore_revision(store, memo_id, &revision, now)?),

        ContactTrust { user } => {
            let (trust, fingerprint) = sync::contact_trust(store, account.user_id, &user)?;
            reply(json!({ "trust": trust, "fingerprint": fingerprint }))
        }
        PrepareShare { memo_id, user, permission, now } => {
            reply(sync::prepare_share(store, account, memo_id, &user, permission, now)?)
        }
        PrepareRevoke { memo_id, user_ids } => {
            let prepared = sync::prepare_revoke(store, account, memo_id, &user_ids)?;
            let request = serde_json::to_value(&prepared.request).expect("serializable");
            c.revokes.retain(|r| r.memo_id != memo_id);
            c.revokes.push(prepared);
            reply(request)
        }
        ApplyRevoke { memo_id, response } => {
            let index = c
                .revokes
                .iter()
                .position(|r| r.memo_id == memo_id)
                .ok_or_else(|| Fail::new("no_revoke", "no revoke prepared for this memo"))?;
            let prepared = c.revokes.swap_remove(index);
            reply(sync::apply_revoke(&mut c.store, prepared, response)?)
        }
        VerifyContact { user_id } => reply(sync::verify_contact(store, user_id)?),

        _ => unreachable!("handled before"),
    }
}

fn client(state: &mut State) -> Result<&mut Client> {
    state.client.as_mut().ok_or_else(|| Fail::new("locked", "no account is open"))
}

fn db_path(state: &State, user_id: Uuid) -> Result<PathBuf> {
    let dir = state.dir.as_ref().ok_or_else(|| Fail::new("no_init", "init was not called"))?;
    Ok(dir.join(format!("{user_id}.db")))
}

fn open(state: &mut State, account: UnlockedAccount) -> Result<Reply> {
    let store = SqliteStore::open(&db_path(state, account.user_id)?)?;
    let opened = Opened {
        user_id: account.user_id,
        fingerprint: memo_core::fingerprint(account.user_id, account.keypair.public_key()),
        secrets: Secrets {
            user_id: account.user_id,
            account_key: account.account_key.to_b64(),
            private_key: b64_encode(account.keypair.secret_bytes().as_slice()),
        },
    };
    state.client = Some(Client { account, store, views: Views::new(), session: None, revokes: Vec::new() });
    reply(opened)
}

/// The databases hold decrypted memos and their keys: the directory is for this user only.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn kdf_config(salt: Salt, params: KdfParams) -> KdfConfig {
    KdfConfig { salt, params }
}

/// The password keys for the KDF parameters in a prelogin response (bounds checked).
fn password_keys(password: &str, prelogin: &PreloginResponse) -> Result<PasswordKeys> {
    Ok(memo_core::derive_password_keys(password, &prelogin.kdf.salt, prelogin.kdf.params)?)
}

#[cfg(test)]
mod tests;
