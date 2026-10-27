//! `memo-core` for the web app, compiled to WASM. The page's worker owns one [`Client`]: the
//! unlocked account and an in-memory store. It does the HTTP itself and passes the bodies through.
//!
//! JSON crosses the boundary as strings (the `memo_core::api` wire format, or the read models in
//! `memo_core::sync`), bytes as `Uint8Array`. Errors are JS `Error`s whose `name` is a stable code
//! (see [`code`]).

use memo_core::{
    AttachmentKind, KdfParams, MemoPayload, PasswordKeys, RecoveryKey, UnlockedAccount,
    api::{
        DeleteAccountRequest, DeviceInfo, KdfConfig, LoginRequest, LoginResponse, NewPassword, PasswordChangeRequest,
        Permission, PreloginResponse, RecoverCompleteRequest, RecoverRequest, RecoverResponse, RegisterRequest,
        Revision, RevokeResponse, SyncResponse, UserInfo,
    },
    image, render,
    sync::{self, MemoFilter, MemoryStore, NewAttachment, NewThumbnail, PreparedRevoke, Step, Store, SyncSession},
};
use serde::Serialize;
use uuid::Uuid;
use wasm_bindgen::prelude::*;

type JsResult<T> = Result<T, JsValue>;

fn error(name: &str, message: impl std::fmt::Display) -> JsValue {
    let error = js_sys::Error::new(&message.to_string());
    error.set_name(name);
    error.into()
}

/// The `name` of the thrown error, for the UI to branch on.
fn code(e: &memo_core::Error) -> &'static str {
    use memo_core::Error::*;
    match e {
        Decrypt => "decrypt",
        UnsupportedFormat(_) | UnsupportedPayloadVersion(_) => "newer_format",
        Malformed(_) => "malformed",
        KdfParamsOutOfBounds => "kdf_params",
        PasswordTooShort => "password_too_short",
        RecoveryKeyInvalid | RecoveryKeyChecksum => "recovery_key_invalid",
        KeyMismatch => "key_mismatch",
        NotFound => "not_found",
        NotOwner => "not_owner",
        ReadOnly => "read_only",
        InConflict => "in_conflict",
        KeyChanged(_) => "key_changed",
        NotPinned(_) => "not_pinned",
        Store(_) => "store",
    }
}

fn core(e: memo_core::Error) -> JsValue {
    error(code(&e), &e)
}

fn parse<T: serde::de::DeserializeOwned>(json: &str) -> JsResult<T> {
    serde_json::from_str(json).map_err(|e| error("bad_json", e))
}

fn json(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("serializable")
}

fn id(text: &str) -> JsResult<Uuid> {
    Uuid::try_parse(text).map_err(|e| error("bad_id", e))
}

fn kind(text: &str) -> JsResult<AttachmentKind> {
    match text {
        "original" => Ok(AttachmentKind::Original),
        "thumbnail" => Ok(AttachmentKind::Thumbnail),
        _ => Err(error("bad_kind", text)),
    }
}

fn kind_str(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Original => "original",
        AttachmentKind::Thumbnail => "thumbnail",
    }
}

fn kdf_config(salt: memo_core::Salt, params: KdfParams) -> KdfConfig {
    KdfConfig { salt, params }
}

/// Derives the password keys for the KDF parameters in a prelogin response (bounds checked).
fn password_keys(password: &str, prelogin: &str) -> JsResult<PasswordKeys> {
    let prelogin: PreloginResponse = parse(prelogin)?;
    memo_core::derive_password_keys(password, &prelogin.kdf.salt, prelogin.kdf.params).map_err(core)
}

// ---- Stateless helpers ----

/// Why the password is not accepted, or `undefined`.
#[wasm_bindgen]
pub fn password_problem(password: &str) -> Option<String> {
    memo_core::check_password_policy(password).err().map(|e| e.to_string())
}

/// Safe HTML for a memo body (see `memo_core::render`).
#[wasm_bindgen]
pub fn render_markdown(body: &str) -> String {
    render::to_html(body)
}

/// Whether decrypted bytes may be shown inline as `mime`.
#[wasm_bindgen]
pub fn image_renderable(bytes: &[u8], mime: &str) -> bool {
    image::renderable_as(bytes, mime)
}

#[wasm_bindgen]
pub struct CleanImage {
    inner: image::CleanImage,
}

#[wasm_bindgen]
impl CleanImage {
    pub fn mime(&self) -> String {
        self.inner.kind.mime().into()
    }
    pub fn width(&self) -> u32 {
        self.inner.width
    }
    pub fn height(&self) -> u32 {
        self.inner.height
    }
    pub fn orientation(&self) -> u16 {
        self.inner.orientation
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.inner.bytes.clone()
    }
}

/// Type check, dimensions, orientation and metadata removal for an image to attach.
#[wasm_bindgen]
pub fn clean_image(bytes: &[u8]) -> JsResult<CleanImage> {
    image::clean_image(bytes).map(|inner| CleanImage { inner }).map_err(core)
}

// ---- Account flows ----

/// A new account, before the server accepted it. Holds the keys until then.
#[wasm_bindgen]
pub struct Registration {
    inner: memo_core::Registration,
}

#[wasm_bindgen]
impl Registration {
    /// Generates the keys. Slow: runs Argon2.
    #[wasm_bindgen(constructor)]
    pub fn new(password: &str) -> JsResult<Registration> {
        memo_core::register(password).map(|inner| Registration { inner }).map_err(core)
    }

    /// Body of `POST /auth/register`.
    pub fn request(&self, username: &str, invite_code: &str, device: &str) -> JsResult<String> {
        let r = &self.inner;
        Ok(json(&RegisterRequest {
            invite_code: invite_code.trim().into(),
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
            device: parse::<DeviceInfo>(device)?,
        }))
    }

    /// To show once; the user must keep it.
    pub fn recovery_key(&self) -> String {
        self.inner.recovery_key.to_display().to_string()
    }

    /// After a `201`.
    pub fn into_client(self) -> Client {
        Client::new(self.inner.account)
    }
}

/// Password keys for a login, from the prelogin response. Slow: runs Argon2.
#[wasm_bindgen]
pub struct PasswordLogin {
    keys: PasswordKeys,
}

#[wasm_bindgen]
impl PasswordLogin {
    #[wasm_bindgen(constructor)]
    pub fn new(password: &str, prelogin: &str) -> JsResult<PasswordLogin> {
        Ok(PasswordLogin { keys: password_keys(password, prelogin)? })
    }

    /// Body of `POST /auth/login`.
    pub fn request(&self, username: &str, device: &str) -> JsResult<String> {
        Ok(json(&LoginRequest {
            username: username.trim().into(),
            auth_key: self.keys.auth_key.clone(),
            device: parse(device)?,
        }))
    }

    /// Decrypts the account from the login response.
    pub fn unlock(&self, response: &str) -> JsResult<Client> {
        let r: LoginResponse = parse(response)?;
        memo_core::unlock(r.user_id, &self.keys.master_key, &r.public_key, &r.enc_account_key, &r.enc_private_key)
            .map(Client::new)
            .map_err(core)
    }
}

/// Account recovery with the recovery key (`docs/crypto.md#account-flows`).
#[wasm_bindgen]
pub struct Recovery {
    key: RecoveryKey,
}

#[wasm_bindgen]
impl Recovery {
    #[wasm_bindgen(constructor)]
    pub fn new(recovery_key: &str) -> JsResult<Recovery> {
        RecoveryKey::parse(recovery_key).map(|key| Recovery { key }).map_err(core)
    }

    /// Body of `POST /auth/recover`.
    pub fn request(&self, username: &str) -> String {
        json(&RecoverRequest { username: username.trim().into(), recovery_auth_key: self.key.auth_key() })
    }

    /// Unlocks the account from the recover response and prepares the new password and recovery
    /// key. Slow: runs Argon2.
    pub fn complete(&self, response: &str, new_password: &str, device: &str) -> JsResult<RecoveryCompletion> {
        let r: RecoverResponse = parse(response)?;
        let account_key =
            memo_core::recover_account_key(r.user_id, &self.key, &r.enc_account_key_recovery).map_err(core)?;
        let account = memo_core::unlock_with_account_key(r.user_id, account_key, &r.public_key, &r.enc_private_key)
            .map_err(core)?;
        let password = account.change_password(new_password).map_err(core)?;
        let recovery = account.new_recovery_key();
        let request = json(&RecoverCompleteRequest {
            recovery_token: r.recovery_token,
            kdf: kdf_config(password.kdf_salt, password.kdf),
            auth_key: password.auth_key,
            enc_account_key: password.enc_account_key,
            recovery_auth_key: recovery.recovery_auth_key,
            enc_account_key_recovery: recovery.enc_account_key_recovery,
            device: parse(device)?,
        });
        Ok(RecoveryCompletion { request, recovery_key: recovery.recovery_key.to_display().to_string(), account })
    }
}

#[wasm_bindgen]
pub struct RecoveryCompletion {
    request: String,
    recovery_key: String,
    account: UnlockedAccount,
}

#[wasm_bindgen]
impl RecoveryCompletion {
    /// Body of `POST /auth/recover/complete`.
    pub fn request(&self) -> String {
        self.request.clone()
    }

    /// The new recovery key, to show once.
    pub fn recovery_key(&self) -> String {
        self.recovery_key.clone()
    }

    pub fn into_client(self) -> Client {
        Client::new(self.account)
    }
}

// ---- The client ----

#[wasm_bindgen]
pub struct Client {
    account: UnlockedAccount,
    store: MemoryStore,
    session: Option<SyncSession>,
    revokes: Vec<PreparedRevoke>,
}

#[derive(Serialize)]
struct UploadJson {
    memo_id: Uuid,
    attachment_id: Uuid,
    kind: &'static str,
}

#[derive(Serialize)]
struct TrustJson {
    trust: sync::Trust,
    fingerprint: String,
}

#[wasm_bindgen]
impl Client {
    fn new(account: UnlockedAccount) -> Self {
        Self { account, store: MemoryStore::new(), session: None, revokes: Vec::new() }
    }

    pub fn user_id(&self) -> String {
        self.account.user_id.to_string()
    }

    /// The fingerprint other users compare to verify this account's key.
    pub fn fingerprint(&self) -> String {
        memo_core::fingerprint(self.account.user_id, self.account.keypair.public_key())
    }

    // -- Sync --

    /// Starts a sync session. Returns the body of the first `POST /sync`.
    pub fn sync_start(&mut self, now: f64) -> JsResult<String> {
        let session = SyncSession::start(&mut self.store, &self.account, now as i64).map_err(core)?;
        let request = json(session.request());
        self.session = Some(session);
        Ok(request)
    }

    /// Applies a sync response. Returns the next request body, or `undefined` when done.
    pub fn sync_handle(&mut self, response: &str) -> JsResult<Option<String>> {
        let response: SyncResponse = parse(response)?;
        let session = self.session.as_mut().ok_or_else(|| error("no_session", "no sync in progress"))?;
        match session.handle(&mut self.store, &self.account, response).map_err(core)? {
            Step::Send => Ok(Some(json(session.request()))),
            Step::Done => Ok(None),
        }
    }

    /// Ends the session (also after a network error). Returns its events as JSON.
    pub fn sync_finish(&mut self) -> String {
        let events = self.session.take().map(|s| s.into_report().events).unwrap_or_default();
        json(&events)
    }

    /// Blobs to `PUT`, as JSON `[{memo_id, attachment_id, kind}]`.
    pub fn pending_uploads(&self) -> JsResult<String> {
        let uploads = sync::pending_uploads(&self.store).map_err(core)?;
        let uploads: Vec<UploadJson> = uploads
            .into_iter()
            .map(|u| UploadJson { memo_id: u.memo_id, attachment_id: u.attachment_id, kind: kind_str(u.kind) })
            .collect();
        Ok(json(&uploads))
    }

    /// The encrypted blob, as uploaded and downloaded.
    pub fn blob(&self, attachment_id: &str, kind: &str) -> JsResult<Option<Vec<u8>>> {
        self.store.blob(id(attachment_id)?, self::kind(kind)?).map_err(core)
    }

    pub fn upload_done(&mut self, attachment_id: &str, kind: &str) -> JsResult<()> {
        sync::upload_done(&mut self.store, id(attachment_id)?, self::kind(kind)?).map_err(core)
    }

    /// Caches a downloaded blob.
    pub fn put_blob(&mut self, attachment_id: &str, kind: &str, envelope: &[u8]) -> JsResult<()> {
        self.store.put_blob(id(attachment_id)?, self::kind(kind)?, envelope).map_err(core)
    }

    /// Anything local the server does not have yet. Closing the tab would lose it.
    pub fn has_unsynced(&self) -> JsResult<bool> {
        let s = &self.store;
        Ok(!s.dirty_memo_ids().map_err(core)?.is_empty()
            || !s.dirty_state_ids().map_err(core)?.is_empty()
            || s.vault().map_err(core)?.is_some_and(|v| v.local.is_some())
            || s.pending_attachments().map_err(core)?.iter().any(|a| a.upload_original || a.upload_thumbnail))
    }

    // -- Read models --

    /// `MemoFilter` JSON in, `MemoSummary[]` JSON out.
    pub fn memo_list(&self, filter: &str) -> JsResult<String> {
        let filter: MemoFilter = parse(filter)?;
        sync::memo_list(&self.store, &filter).map(|l| json(&l)).map_err(core)
    }

    pub fn tag_counts(&self) -> JsResult<String> {
        sync::tag_counts(&self.store).map(|t| json(&t)).map_err(core)
    }

    /// `MemoDetail` JSON.
    pub fn memo_detail(&self, memo_id: &str) -> JsResult<String> {
        sync::memo_detail(&self.store, self.account.user_id, id(memo_id)?).map(|d| json(&d)).map_err(core)
    }

    /// Decrypted attachment bytes, or `undefined` if the blob is not cached yet (download it,
    /// [`put_blob`](Self::put_blob), and ask again). Check them with [`image_renderable`].
    pub fn attachment(&self, memo_id: &str, attachment_id: &str, kind: &str) -> JsResult<Option<Vec<u8>>> {
        let (memo_id, attachment_id) = (id(memo_id)?, id(attachment_id)?);
        let memo = self.store.memo(memo_id).map_err(core)?.ok_or_else(|| core(memo_core::Error::NotFound))?;
        let payloads = [
            memo.local.as_ref(),
            memo.base.as_ref(),
            memo.conflict.as_ref().map(|c| &c.theirs),
            memo.undecoded.as_ref().and_then(|u| u.view.as_ref()),
        ];
        let attachment = payloads
            .into_iter()
            .flatten()
            .flat_map(|p| &p.attachments)
            .find(|a| a.id == attachment_id)
            .ok_or_else(|| core(memo_core::Error::NotFound))?;
        sync::cached_attachment(&self.store, attachment, self::kind(kind)?).map_err(core)
    }

    // -- Local edits --

    /// Returns the new memo's id.
    pub fn create_memo(&mut self, body: &str, now: f64) -> JsResult<String> {
        let payload = MemoPayload::new(body, now as i64);
        sync::create_memo(&mut self.store, &self.account, payload, now as i64).map(|id| id.to_string()).map_err(core)
    }

    /// Replaces the body. `prune` also drops attachments the body no longer refers to; pass it when
    /// the editor closes.
    pub fn edit_memo(&mut self, memo_id: &str, body: &str, now: f64, prune: bool) -> JsResult<()> {
        let memo_id = id(memo_id)?;
        let memo = self.store.memo(memo_id).map_err(core)?.ok_or_else(|| core(memo_core::Error::NotFound))?;
        let current = memo.current().ok_or_else(|| core(memo_core::Error::ReadOnly))?;
        let mut payload = current.clone();
        if payload.body != body {
            payload.body = body.into();
            payload.edited_at = now as i64;
        }
        if prune {
            payload.prune_attachments();
        }
        if &payload == current {
            return Ok(());
        }
        sync::edit_memo(&mut self.store, memo_id, payload).map_err(core)
    }

    /// Flips the task box at `offset` (from the rendered `data-task`). False if the body changed.
    pub fn toggle_task(&mut self, memo_id: &str, offset: usize, now: f64) -> JsResult<bool> {
        let memo = self.store.memo(id(memo_id)?).map_err(core)?.ok_or_else(|| core(memo_core::Error::NotFound))?;
        let body = memo.current().map(|p| p.body.clone()).unwrap_or_default();
        match render::toggle_task(&body, offset) {
            Some(body) => self.edit_memo(memo_id, &body, now, false).map(|()| true),
            None => Ok(false),
        }
    }

    pub fn delete_memo(&mut self, memo_id: &str) -> JsResult<()> {
        sync::delete_memo(&mut self.store, &self.account, id(memo_id)?).map_err(core)
    }

    pub fn set_memo_state(&mut self, memo_id: &str, pinned: bool, archived: bool) -> JsResult<()> {
        sync::set_memo_state(&mut self.store, id(memo_id)?, pinned, archived).map_err(core)
    }

    /// Settles a conflict with `body`, keeping the images either side has that it refers to.
    pub fn resolve_conflict(&mut self, memo_id: &str, body: &str, now: f64) -> JsResult<()> {
        let memo_id = id(memo_id)?;
        let memo = self.store.memo(memo_id).map_err(core)?.ok_or_else(|| core(memo_core::Error::NotFound))?;
        let theirs = memo.conflict.as_ref().map(|c| c.theirs.clone()).ok_or_else(|| error("no_conflict", memo_id))?;
        let mut resolved = MemoPayload::new(body, now as i64);
        for attachment in memo.current().into_iter().flat_map(|p| &p.attachments).chain(&theirs.attachments) {
            if !resolved.attachments.iter().any(|a| a.id == attachment.id) {
                resolved.attachments.push(attachment.clone());
            }
        }
        resolved.prune_attachments();
        if resolved.body == theirs.body && resolved.attachments == theirs.attachments {
            resolved = theirs;
        }
        sync::resolve_conflict(&mut self.store, memo_id, resolved).map_err(core)
    }

    /// Returns the copy's id.
    pub fn save_as_copy(&mut self, memo_id: &str, now: f64) -> JsResult<String> {
        sync::save_as_copy(&mut self.store, &self.account, id(memo_id)?, now as i64)
            .map(|id| id.to_string())
            .map_err(core)
    }

    /// Encrypts and attaches an image (already cleaned, see [`clean_image`]). Returns the
    /// attachment id; the caller then inserts `![](attachment:<id>)` with [`edit_memo`](Self::edit_memo).
    #[allow(clippy::too_many_arguments)]
    pub fn add_attachment(
        &mut self,
        memo_id: &str,
        mime: &str,
        width: u32,
        height: u32,
        bytes: &[u8],
        thumb_mime: Option<String>,
        thumb_width: u32,
        thumb_height: u32,
        thumb: Option<Vec<u8>>,
    ) -> JsResult<String> {
        if !image::renderable_as(bytes, mime) {
            return Err(error("bad_image", "the bytes are not the stated image type"));
        }
        let thumbnail = match (&thumb_mime, &thumb) {
            (Some(mime), Some(bytes)) if image::renderable_as(bytes, mime) => {
                Some(NewThumbnail { mime: mime.clone(), width: thumb_width, height: thumb_height, bytes })
            }
            (None, None) => None,
            _ => return Err(error("bad_image", "the thumbnail is not the stated image type")),
        };
        let new = NewAttachment { mime: mime.into(), width, height, bytes, thumbnail };
        sync::add_attachment(&mut self.store, id(memo_id)?, new).map(|a| a.id.to_string()).map_err(core)
    }

    // -- Revisions --

    /// The body of a revision from `GET /memos/{id}/revisions/{version}`.
    pub fn open_revision(&self, memo_id: &str, revision: &str) -> JsResult<String> {
        let revision: Revision = parse(revision)?;
        sync::open_revision(&self.store, id(memo_id)?, &revision).map(|p| p.body).map_err(core)
    }

    /// Makes an old revision the working copy; the next sync pushes it as a new version.
    pub fn restore_revision(&mut self, memo_id: &str, revision: &str, now: f64) -> JsResult<()> {
        let revision: Revision = parse(revision)?;
        let memo_id = id(memo_id)?;
        let mut payload = sync::open_revision(&self.store, memo_id, &revision).map_err(core)?;
        payload.edited_at = now as i64;
        sync::edit_memo(&mut self.store, memo_id, payload).map_err(core)
    }

    // -- Sharing --

    /// `{trust, fingerprint}` for a user from `GET /users/lookup`, before sharing.
    pub fn contact_trust(&self, user: &str) -> JsResult<String> {
        let user: UserInfo = parse(user)?;
        let (trust, fingerprint) = sync::contact_trust(&self.store, self.account.user_id, &user).map_err(core)?;
        Ok(json(&TrustJson { trust, fingerprint }))
    }

    /// Body of `POST /memos/{id}/members`. Pins the user's key if new; fails with `key_changed`.
    pub fn prepare_share(&mut self, memo_id: &str, user: &str, permission: &str, now: f64) -> JsResult<String> {
        let user: UserInfo = parse(user)?;
        let permission: Permission = parse(&json(&permission))?;
        sync::prepare_share(&mut self.store, &self.account, id(memo_id)?, &user, permission, now as i64)
            .map(|r| json(&r))
            .map_err(core)
    }

    /// Body of `POST /memos/{id}/revoke`. `user_ids` is a JSON array.
    pub fn prepare_revoke(&mut self, memo_id: &str, user_ids: &str) -> JsResult<String> {
        let user_ids: Vec<Uuid> = parse(user_ids)?;
        let prepared = sync::prepare_revoke(&self.store, &self.account, id(memo_id)?, &user_ids).map_err(core)?;
        let request = json(&prepared.request);
        self.revokes.retain(|r| r.memo_id != prepared.memo_id);
        self.revokes.push(prepared);
        Ok(request)
    }

    /// After the revoke's `200`.
    pub fn apply_revoke(&mut self, memo_id: &str, response: &str) -> JsResult<()> {
        let memo_id = id(memo_id)?;
        let response: RevokeResponse = parse(response)?;
        let index = self
            .revokes
            .iter()
            .position(|r| r.memo_id == memo_id)
            .ok_or_else(|| error("no_revoke", "no revoke prepared for this memo"))?;
        let prepared = self.revokes.swap_remove(index);
        sync::apply_revoke(&mut self.store, prepared, response).map_err(core)
    }

    /// After the user compared fingerprints.
    pub fn verify_contact(&mut self, user_id: &str) -> JsResult<()> {
        sync::verify_contact(&mut self.store, id(user_id)?).map_err(core)
    }

    // -- Account --

    /// Body of `POST /auth/password`. `prelogin` is this user's prelogin response, for the current
    /// password's KDF parameters. Slow: runs Argon2 twice.
    pub fn password_change(
        &self,
        current_password: &str,
        prelogin: &str,
        new_password: &str,
        revoke_other_devices: bool,
    ) -> JsResult<String> {
        let current = password_keys(current_password, prelogin)?;
        let new = self.account.change_password(new_password).map_err(core)?;
        Ok(json(&PasswordChangeRequest {
            auth_key: current.auth_key,
            new: NewPassword {
                kdf: kdf_config(new.kdf_salt, new.kdf),
                auth_key: new.auth_key,
                enc_account_key: new.enc_account_key,
            },
            revoke_other_devices,
        }))
    }

    /// Body of `DELETE /me`. Slow: runs Argon2.
    pub fn delete_account(&self, password: &str, prelogin: &str) -> JsResult<String> {
        Ok(json(&DeleteAccountRequest { auth_key: password_keys(password, prelogin)?.auth_key }))
    }
}
