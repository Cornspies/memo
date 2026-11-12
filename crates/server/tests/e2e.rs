//! The real client code (`memo-core`) against the real router and SQLite.

use std::path::PathBuf;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use memo_core::{
    AttachmentKind, RecoveryKey, UnlockedAccount,
    api::*,
    derive_password_keys,
    sync::{self as client, MemoryStore, Step, Store, SyncEvent, SyncSession},
    unlock,
};
use memo_server::{AppState, Config, admin, jobs};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct horse battery staple";

struct TestServer {
    app: memo_server::App,
    router: Router,
    dir: PathBuf,
}

impl TestServer {
    fn new() -> Self {
        Self::at(std::env::temp_dir().join(format!("memo-test-{}", Uuid::now_v7())))
    }

    fn at(dir: PathBuf) -> Self {
        let config = Config { data_dir: dir.clone(), public_url: "https://memo.test".into(), ..Config::default() };
        let app = AppState::open(config).unwrap();
        Self { router: memo_server::router(app.clone()), app, dir }
    }

    async fn send(&self, request: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let response = self.router.clone().oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        (parts.status, parts.headers, body.collect().await.unwrap().to_bytes().to_vec())
    }

    async fn call(&self, method: Method, path: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(format!("/api/v1{path}"));
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => request.header(header::CONTENT_TYPE, "application/json").body(Body::from(body.to_string())),
            None => request.body(Body::empty()),
        };
        let (status, _, bytes) = self.send(request.unwrap()).await;
        (status, if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap_or(Value::Null) })
    }

    fn invite(&self) -> String {
        admin::invite_create(&self.app.db, "user", 3600 * 1000).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Debug)]
struct Client {
    username: String,
    token: String,
    account: UnlockedAccount,
    store: MemoryStore,
}

fn device(kind: DeviceKind) -> DeviceInfo {
    DeviceInfo { name: "test device".into(), kind }
}

async fn register(server: &TestServer, username: &str) -> (Client, RecoveryKey) {
    let reg = memo_core::register(PASSWORD).unwrap();
    let request = RegisterRequest {
        invite_code: server.invite(),
        user_id: reg.user_id,
        username: username.into(),
        kdf: KdfConfig { salt: reg.kdf_salt, params: reg.kdf },
        auth_key: reg.auth_key.clone(),
        recovery_auth_key: reg.recovery_auth_key.clone(),
        public_key: reg.public_key,
        enc_account_key: reg.enc_account_key.clone(),
        enc_account_key_recovery: reg.enc_account_key_recovery.clone(),
        enc_private_key: reg.enc_private_key.clone(),
        vault: reg.vault.clone(),
        device: device(DeviceKind::Desktop),
    };
    let (status, body) = server.call(Method::POST, "/auth/register", None, Some(json!(request))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session: SessionResponse = serde_json::from_value(body).unwrap();
    let client = Client {
        username: username.into(),
        token: session.token.unwrap(),
        account: reg.account,
        store: MemoryStore::new(),
    };
    (client, reg.recovery_key)
}

/// A second device of the same user, through prelogin + login + unlock.
async fn login(server: &TestServer, username: &str, password: &str) -> Result<Client, StatusCode> {
    let (_, body) = server.call(Method::POST, "/auth/prelogin", None, Some(json!({ "username": username }))).await;
    let prelogin: PreloginResponse = serde_json::from_value(body).unwrap();
    let keys = derive_password_keys(password, &prelogin.kdf.salt, prelogin.kdf.params).unwrap();
    let request =
        LoginRequest { username: username.into(), auth_key: keys.auth_key, device: device(DeviceKind::Android) };
    let (status, body) = server.call(Method::POST, "/auth/login", None, Some(json!(request))).await;
    if status != StatusCode::OK {
        return Err(status);
    }
    let login: LoginResponse = serde_json::from_value(body).unwrap();
    let account =
        unlock(login.user_id, &keys.master_key, &login.public_key, &login.enc_account_key, &login.enc_private_key)
            .unwrap();
    Ok(Client { username: username.into(), token: login.token.unwrap(), account, store: MemoryStore::new() })
}

impl Client {
    async fn sync(&mut self, server: &TestServer) -> Vec<SyncEvent> {
        let mut session = SyncSession::start(&mut self.store, &self.account, 1000).unwrap();
        loop {
            let (status, body) =
                server.call(Method::POST, "/sync", Some(&self.token), Some(json!(session.request()))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let response: SyncResponse = serde_json::from_value(body).unwrap();
            if session.handle(&mut self.store, &self.account, response).unwrap() == Step::Done {
                break;
            }
        }
        for upload in client::pending_uploads(&self.store).unwrap() {
            let kind = if upload.kind == AttachmentKind::Original { "original" } else { "thumbnail" };
            let blob = self.store.blob(upload.attachment_id, upload.kind).unwrap().unwrap();
            let (status, ..) = self.put_blob(server, upload.memo_id, upload.attachment_id, kind, blob).await;
            assert!(status.is_success(), "{status}");
            client::upload_done(&mut self.store, upload.attachment_id, upload.kind).unwrap();
        }
        session.into_report().events
    }

    async fn put_blob(
        &self,
        server: &TestServer,
        memo_id: Uuid,
        id: Uuid,
        kind: &str,
        blob: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let request = Request::put(format!("/api/v1/memos/{memo_id}/attachments/{id}/{kind}"))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::from(blob))
            .unwrap();
        server.send(request).await
    }

    async fn get_blob(&self, server: &TestServer, id: Uuid, etag: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut request = Request::get(format!("/api/v1/attachments/{id}/original"))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
        if let Some(etag) = etag {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        server.send(request.body(Body::empty()).unwrap()).await
    }

    fn create(&mut self, body: &str) -> Uuid {
        client::create_memo(&mut self.store, &self.account, memo_core::MemoPayload::new(body, 1), 1).unwrap()
    }

    fn edit(&mut self, id: Uuid, body: &str) {
        let mut payload = self.memo(id).current().unwrap().clone();
        payload.body = body.into();
        client::edit_memo(&mut self.store, id, payload).unwrap();
    }

    fn memo(&self, id: Uuid) -> client::MemoRecord {
        self.store.memo(id).unwrap().expect("memo exists")
    }

    fn body(&self, id: Uuid) -> String {
        self.memo(id).current().unwrap().body.clone()
    }

    async fn share(&mut self, server: &TestServer, id: Uuid, with: &str, permission: Permission) {
        let (status, body) =
            server.call(Method::GET, &format!("/users/lookup?username={with}"), Some(&self.token), None).await;
        assert_eq!(status, StatusCode::OK);
        let user: UserInfo = serde_json::from_value(body).unwrap();
        let request = client::prepare_share(&mut self.store, &self.account, id, &user, permission, 1).unwrap();
        let (status, body) =
            server.call(Method::POST, &format!("/memos/{id}/members"), Some(&self.token), Some(json!(request))).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
    }
}

#[tokio::test]
async fn register_prelogin_login() {
    let server = TestServer::new();
    let (alice, _) = register(&server, "Alice").await;
    assert_eq!(alice.username, "Alice");
    let (status, body) = server.call(Method::GET, "/me", Some(&alice.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["username"], "alice");

    let second = login(&server, "alice", PASSWORD).await.unwrap();
    assert_eq!(second.account.user_id, alice.account.user_id);
    assert_eq!(second.account.account_key, alice.account.account_key);
    assert_eq!(login(&server, "alice", "not the password at all").await.unwrap_err(), StatusCode::UNAUTHORIZED);
    assert_eq!(login(&server, "nobody", PASSWORD).await.unwrap_err(), StatusCode::UNAUTHORIZED);

    // Unknown users get a stable fake salt.
    let prelogin =
        |name: &'static str| server.call(Method::POST, "/auth/prelogin", None, Some(json!({ "username": name })));
    let (fake1, fake2, real) = (prelogin("nobody").await.1, prelogin("nobody").await.1, prelogin("alice").await.1);
    assert_eq!(fake1, fake2);
    assert_ne!(fake1["kdf"]["salt"], real["kdf"]["salt"]);

    let (status, body) = server.call(Method::GET, "/devices", Some(&alice.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);

    // Logging in with a valid session of the same user replaces that device.
    let salt = prelogin_salt(&server, "alice").await;
    let keys = derive_password_keys(PASSWORD, &salt, memo_core::KdfParams::DEFAULT).unwrap();
    let request = LoginRequest { username: "alice".into(), auth_key: keys.auth_key, device: device(DeviceKind::Web) };
    let (status, _) = server.call(Method::POST, "/auth/login", Some(&second.token), Some(json!(request))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(server.call(Method::GET, "/me", Some(&second.token), None).await.0, StatusCode::UNAUTHORIZED);
    let (_, body) = server.call(Method::GET, "/devices", Some(&alice.token), None).await;
    assert_eq!(body.as_array().unwrap().len(), 2);
    let second = login(&server, "alice", PASSWORD).await.unwrap();

    let (status, _) = server.call(Method::POST, "/auth/logout", Some(&second.token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, body) = server.call(Method::GET, "/me", Some(&second.token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthenticated");

    // Invites are single-use.
    let code = server.invite();
    let reg = memo_core::register(PASSWORD).unwrap();
    let mut request = json!({
        "invite_code": code, "user_id": reg.user_id, "username": "bob",
        "kdf": KdfConfig { salt: reg.kdf_salt, params: reg.kdf }, "auth_key": reg.auth_key,
        "recovery_auth_key": reg.recovery_auth_key, "public_key": reg.public_key,
        "enc_account_key": memo_core::b64_encode(&reg.enc_account_key),
        "enc_account_key_recovery": memo_core::b64_encode(&reg.enc_account_key_recovery),
        "enc_private_key": memo_core::b64_encode(&reg.enc_private_key), "vault": memo_core::b64_encode(&reg.vault),
        "device": device(DeviceKind::Desktop),
    });
    assert_eq!(server.call(Method::POST, "/auth/register", None, Some(request.clone())).await.0, StatusCode::CREATED);
    request["user_id"] = json!(Uuid::now_v7());
    request["username"] = json!("carol");
    let (status, body) = server.call(Method::POST, "/auth/register", None, Some(request)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "invite_invalid");
}

#[tokio::test]
async fn devices_sync_share_and_revoke() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let (mut bob, _) = register(&server, "bob").await;
    let mut alice2 = login(&server, "alice", PASSWORD).await.unwrap();

    let id = alice.create("one\ntwo\nthree\n");
    assert!(alice.sync(&server).await.is_empty());
    assert!(alice2.sync(&server).await.is_empty());
    assert_eq!(alice2.body(id), "one\ntwo\nthree\n");

    // Concurrent edits on two devices merge through the real server.
    alice.edit(id, "ONE\ntwo\nthree\n");
    alice2.edit(id, "one\ntwo\nTHREE\n");
    alice.sync(&server).await;
    alice2.sync(&server).await;
    alice.sync(&server).await;
    assert_eq!(alice.body(id), "ONE\ntwo\nTHREE\n");
    assert!(!alice.memo(id).is_dirty() && !alice2.memo(id).is_dirty());

    alice.share(&server, id, "bob", Permission::Write).await;
    assert!(bob.sync(&server).await.is_empty());
    assert_eq!(bob.body(id), "ONE\ntwo\nTHREE\n");
    bob.edit(id, "ONE\ntwo\nTHREE\nbob");
    bob.sync(&server).await;
    alice2.sync(&server).await;
    assert_eq!(alice2.body(id), "ONE\ntwo\nTHREE\nbob");

    let (status, body) = server.call(Method::GET, &format!("/memos/{id}/revisions"), Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["version"], 4);
    let (status, body) = server.call(Method::GET, &format!("/memos/{id}/revisions/1"), Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Read-only now: pushes are refused.
    let (status, _) = server
        .call(
            Method::PATCH,
            &format!("/memos/{id}/members/{}", bob.account.user_id),
            Some(&alice.token),
            Some(json!({ "permission": "read" })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    bob.sync(&server).await;
    assert_eq!(
        client::edit_memo(&mut bob.store, id, memo_core::MemoPayload::new("x", 2)),
        Err(memo_core::Error::ReadOnly)
    );

    alice.sync(&server).await;
    let prepared = client::prepare_revoke(&alice.store, &alice.account, id, &[bob.account.user_id]).unwrap();
    let (status, body) = server
        .call(Method::POST, &format!("/memos/{id}/revoke"), Some(&alice.token), Some(json!(prepared.request)))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    client::apply_revoke(&mut alice.store, prepared, serde_json::from_value(body).unwrap()).unwrap();
    bob.sync(&server).await;
    assert!(bob.store.memo(id).unwrap().is_none(), "bob lost access");
    let (status, _) = server.call(Method::GET, &format!("/memos/{id}/revisions"), Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    alice2.sync(&server).await;
    alice2.edit(id, "private again");
    alice2.sync(&server).await;
    alice.sync(&server).await;
    assert_eq!(alice.body(id), "private again");
    assert_eq!(alice.memo(id).key_version, 2);
}

#[tokio::test]
async fn attachments_roundtrip() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let (mut bob, _) = register(&server, "bob").await;
    let id = alice.create("photo");
    let new =
        client::NewAttachment { mime: "image/jpeg".into(), width: 4, height: 3, bytes: b"jpeg bytes", thumbnail: None };
    let attachment = client::add_attachment(&mut alice.store, id, new).unwrap();
    alice.sync(&server).await;

    let (status, headers, blob) = alice.get_blob(&server, attachment.id, None).await;
    assert_eq!(status, StatusCode::OK);
    let plaintext =
        memo_core::open_attachment(&attachment.key, attachment.id, AttachmentKind::Original, &blob).unwrap();
    assert_eq!(plaintext, b"jpeg bytes");
    let etag = headers[header::ETAG].to_str().unwrap().to_owned();
    assert_eq!(alice.get_blob(&server, attachment.id, Some(&etag)).await.0, StatusCode::NOT_MODIFIED);

    assert_eq!(bob.get_blob(&server, attachment.id, None).await.0, StatusCode::NOT_FOUND, "not a member");
    let (status, ..) = bob.put_blob(&server, id, Uuid::now_v7(), "original", vec![0; 100]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    assert_eq!(alice.put_blob(&server, id, attachment.id, "original", blob.clone()).await.0, StatusCode::OK, "retry");
    let mut other = blob.clone();
    other[50] ^= 1;
    assert_eq!(alice.put_blob(&server, id, attachment.id, "original", other).await.0, StatusCode::CONFLICT);

    alice.share(&server, id, "bob", Permission::Read).await;
    bob.sync(&server).await;
    let record = bob.store.attachment(attachment.id).unwrap().unwrap();
    assert!(record.uploaded);
    assert_eq!(bob.get_blob(&server, attachment.id, None).await.2, blob);

    let (_, body) = server.call(Method::GET, "/me", Some(&alice.token), None).await;
    assert!(body["used_bytes"].as_u64().unwrap() > blob.len() as u64);
}

#[tokio::test]
async fn cookie_sessions_need_the_origin() {
    let server = TestServer::new();
    let (alice, _) = register(&server, "alice").await;
    let (_, body) = server.call(Method::POST, "/auth/prelogin", None, Some(json!({ "username": "alice" }))).await;
    let prelogin: PreloginResponse = serde_json::from_value(body).unwrap();
    let keys = derive_password_keys(PASSWORD, &prelogin.kdf.salt, prelogin.kdf.params).unwrap();
    let request = LoginRequest { username: "alice".into(), auth_key: keys.auth_key, device: device(DeviceKind::Web) };
    let (status, headers, body) = server
        .send(
            Request::post("/api/v1/auth/login")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!(request).to_string()))
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert!(body.get("token").is_none());
    let cookie = headers[header::SET_COOKIE].to_str().unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("Secure") && cookie.contains("SameSite=Strict"));
    let cookie = cookie.split(';').next().unwrap().to_owned();

    let me = Request::get("/api/v1/me").header(header::COOKIE, &cookie).body(Body::empty()).unwrap();
    assert_eq!(server.send(me).await.0, StatusCode::OK);
    let sync = |origin: Option<&str>| {
        let mut request = Request::post("/api/v1/sync")
            .header(header::COOKIE, &cookie)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            request = request.header(header::ORIGIN, origin);
        }
        request.body(Body::from(r#"{"cursor":0}"#)).unwrap()
    };
    assert_eq!(server.send(sync(None)).await.0, StatusCode::FORBIDDEN);
    assert_eq!(server.send(sync(Some("https://evil.test"))).await.0, StatusCode::FORBIDDEN);
    let (status, headers, _) = server.send(sync(Some("https://memo.test"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["memo-api"], "1");
    assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    drop(alice);
}

#[tokio::test]
async fn recovery_replaces_the_password_and_revokes_devices() {
    let server = TestServer::new();
    let (mut alice, recovery_key) = register(&server, "alice").await;
    let id = alice.create("survives recovery");
    alice.sync(&server).await;

    let request = RecoverRequest { username: "alice".into(), recovery_auth_key: recovery_key.auth_key() };
    let (status, body) = server.call(Method::POST, "/auth/recover", None, Some(json!(request))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let recover: RecoverResponse = serde_json::from_value(body).unwrap();
    let account_key =
        memo_core::recover_account_key(recover.user_id, &recovery_key, &recover.enc_account_key_recovery).unwrap();
    let account =
        memo_core::unlock_with_account_key(recover.user_id, account_key, &recover.public_key, &recover.enc_private_key)
            .unwrap();
    let password = account.change_password("a completely new password").unwrap();
    let rotation = account.new_recovery_key();
    let request = RecoverCompleteRequest {
        recovery_token: recover.recovery_token.clone(),
        kdf: KdfConfig { salt: password.kdf_salt, params: password.kdf },
        auth_key: password.auth_key,
        enc_account_key: password.enc_account_key,
        recovery_auth_key: rotation.recovery_auth_key,
        enc_account_key_recovery: rotation.enc_account_key_recovery,
        device: device(DeviceKind::Desktop),
    };
    let (status, body) = server.call(Method::POST, "/auth/recover/complete", None, Some(json!(request))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _) = server.call(Method::POST, "/auth/recover/complete", None, Some(json!(request))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "the token is single-use");

    assert_eq!(server.call(Method::GET, "/me", Some(&alice.token), None).await.0, StatusCode::UNAUTHORIZED);
    assert!(login(&server, "alice", PASSWORD).await.is_err());
    let mut fresh = login(&server, "alice", "a completely new password").await.unwrap();
    fresh.sync(&server).await;
    assert_eq!(fresh.body(id), "survives recovery");
}

#[tokio::test]
async fn restore_from_backup_makes_clients_push_back() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let kept = alice.create("kept");
    alice.sync(&server).await;
    let backup_dir = server.dir.with_extension("backup");
    admin::backup(&server.app.db, &server.dir, &backup_dir).unwrap();

    alice.edit(kept, "kept, newer");
    let lost = alice.create("after the backup");
    alice.sync(&server).await;

    let restored_dir = server.dir.with_extension("restored");
    admin::restore(&backup_dir, &restored_dir).unwrap();
    let restored = TestServer::at(restored_dir);
    let events = alice.sync(&restored).await;
    assert!(events.contains(&SyncEvent::Reset));
    assert!(events.contains(&SyncEvent::ServerLostData { memo_id: kept }));
    assert!(events.contains(&SyncEvent::ServerLostData { memo_id: lost }));

    let mut other = login(&restored, "alice", PASSWORD).await.unwrap();
    other.sync(&restored).await;
    assert_eq!(other.body(kept), "kept, newer");
    assert_eq!(other.body(lost), "after the backup");
    let _ = std::fs::remove_dir_all(backup_dir);
}

#[tokio::test]
async fn gc_purges_tombstones_and_old_cursors_reset() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let mut alice2 = login(&server, "alice", PASSWORD).await.unwrap();
    let gone = alice.create("to be deleted");
    let kept = alice.create("kept");
    alice.sync(&server).await;
    alice2.sync(&server).await;
    client::delete_memo(&mut alice.store, &alice.account, gone).unwrap();
    alice.sync(&server).await;
    alice.sync(&server).await;

    let later = memo_server::db::now_ms() + 91 * 24 * 3600 * 1000;
    let report = jobs::gc_at(&server.app.db, &server.app.config, later).unwrap();
    assert!(report.log_rows_purged >= 1, "{report:?}");

    // alice2 never saw the delete; its cursor is now older than what the server kept.
    let events = alice2.sync(&server).await;
    assert!(events.contains(&SyncEvent::Reset));
    assert!(alice2.store.memo(gone).unwrap().is_none());
    assert_eq!(alice2.body(kept), "kept");
    // alice is up to date and does not reset.
    assert!(alice.sync(&server).await.is_empty());
}

#[tokio::test]
async fn deleting_an_account() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let (mut bob, _) = register(&server, "bob").await;
    let id = alice.create("shared");
    alice.sync(&server).await;
    alice.share(&server, id, "bob", Permission::Read).await;
    bob.sync(&server).await;
    assert_eq!(bob.body(id), "shared");

    let keys =
        derive_password_keys(PASSWORD, &prelogin_salt(&server, "alice").await, memo_core::KdfParams::DEFAULT).unwrap();
    let (status, _) =
        server.call(Method::DELETE, "/me", Some(&alice.token), Some(json!({ "auth_key": keys.auth_key }))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(server.call(Method::GET, "/me", Some(&alice.token), None).await.0, StatusCode::UNAUTHORIZED);
    bob.sync(&server).await;
    assert!(bob.store.memo(id).unwrap().is_none());
    let (status, _) = server.call(Method::GET, "/users/lookup?username=alice", Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

async fn prelogin_salt(server: &TestServer, username: &str) -> memo_core::Salt {
    let (_, body) = server.call(Method::POST, "/auth/prelogin", None, Some(json!({ "username": username }))).await;
    serde_json::from_value::<PreloginResponse>(body).unwrap().kdf.salt
}

/// The whole log of a user, as a full sync of a new device gets it.
async fn full_pull(server: &TestServer, client: &Client) -> Vec<Change> {
    let (mut changes, mut cursor) = (Vec::new(), 0);
    loop {
        let body = json!({ "cursor": cursor });
        let (status, body) = server.call(Method::POST, "/sync", Some(&client.token), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let response: SyncResponse = serde_json::from_value(body).unwrap();
        changes.extend(response.changes);
        cursor = response.cursor;
        if !response.more {
            return changes;
        }
    }
}

async fn revoke(server: &TestServer, owner: &mut Client, id: Uuid, users: &[Uuid]) -> StatusCode {
    let prepared = client::prepare_revoke(&owner.store, &owner.account, id, users).unwrap();
    let path = format!("/memos/{id}/revoke");
    let (status, body) = server.call(Method::POST, &path, Some(&owner.token), Some(json!(prepared.request))).await;
    if status == StatusCode::OK {
        client::apply_revoke(&mut owner.store, prepared, serde_json::from_value(body).unwrap()).unwrap();
    }
    status
}

#[tokio::test]
async fn former_members_are_sent_nothing_of_the_memo() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let (mut bob, _) = register(&server, "bob").await;
    let (mut carol, _) = register(&server, "carol").await;
    let id = alice.create("photo");
    let new =
        client::NewAttachment { mime: "image/jpeg".into(), width: 4, height: 3, bytes: b"jpeg bytes", thumbnail: None };
    let attachment = client::add_attachment(&mut alice.store, id, new).unwrap();
    alice.sync(&server).await;
    alice.share(&server, id, "bob", Permission::Read).await;
    alice.share(&server, id, "carol", Permission::Read).await;
    alice.sync(&server).await;
    for member in [&mut bob, &mut carol] {
        member.sync(&server).await;
        client::set_memo_state(&mut member.store, id, true, false).unwrap();
        member.sync(&server).await;
    }

    // Bob leaves (no key rotation), carol is removed.
    let (status, _) = server.call(Method::DELETE, &format!("/memos/{id}/members/me"), Some(&bob.token), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    alice.sync(&server).await;
    assert_eq!(revoke(&server, &mut alice, id, &[carol.account.user_id]).await, StatusCode::OK);
    alice.sync(&server).await;
    alice.edit(id, "written after they were gone");
    alice.sync(&server).await;

    for former in [&bob, &carol] {
        let changes = full_pull(&server, former).await;
        let about_the_memo: Vec<&Change> = changes
            .iter()
            .filter(|c| match c {
                Change::Memo(m) => m.id == id,
                Change::Attachment(a) => a.id == attachment.id,
                Change::State(s) => s.memo_id == id,
                Change::Access(a) => a.memo_id == id,
                Change::Vault(_) => false,
            })
            .collect();
        let [Change::Access(access)] = about_the_memo[..] else { panic!("{}: {about_the_memo:?}", former.username) };
        assert!(access.revoked);
    }

    // A row left in the log by an older server is not rendered either.
    let (memo, user) = (*id.as_bytes(), *bob.account.user_id.as_bytes());
    server
        .app
        .db
        .write_blocking(|tx| {
            tx.execute(
                "INSERT INTO change_log (user_id, entity_type, entity_id, changed_at) VALUES (?1, 'memo', ?2, 0)",
                rusqlite::params![user, memo],
            )?;
            Ok(())
        })
        .unwrap();
    assert!(!full_pull(&server, &bob).await.iter().any(|c| matches!(c, Change::Memo(m) if m.id == id)));
    assert!(bob.sync(&server).await.is_empty());
    assert!(bob.store.memo(id).unwrap().is_none());
    // Members still get everything.
    let mut alice2 = login(&server, "alice", PASSWORD).await.unwrap();
    alice2.sync(&server).await;
    assert_eq!(alice2.body(id), "written after they were gone");
}

async fn login_from(server: &TestServer, username: &str, auth_key: &memo_core::AuthKey, address: &str) -> StatusCode {
    let request =
        LoginRequest { username: username.into(), auth_key: auth_key.clone(), device: device(DeviceKind::Desktop) };
    let request = Request::post("/api/v1/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-forwarded-for", address)
        .body(Body::from(json!(request).to_string()))
        .unwrap();
    server.send(request).await.0
}

#[tokio::test]
async fn login_backoff_is_per_address_and_hides_who_exists() {
    let server = TestServer::new();
    register(&server, "alice").await;
    let salt = prelogin_salt(&server, "alice").await;
    let right = derive_password_keys(PASSWORD, &salt, memo_core::KdfParams::DEFAULT).unwrap().auth_key;
    let wrong = memo_core::AuthKey::from_bytes(&[7; 32]).unwrap();
    let (attacker, home) = ("203.0.113.9", "2001:db8:1:2::5");

    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(login_from(&server, "alice", &wrong, attacker).await.as_u16());
    }
    assert_eq!(statuses, [401, 401, 401, 401, 401, 429]);
    // The attacker's address is held back, alice at home is not.
    assert_eq!(login_from(&server, "Alice", &right, attacker).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(login_from(&server, "alice", &right, home).await, StatusCode::OK);

    // A name nobody has answers the same way, so the backoff does not show who exists.
    let mut statuses = Vec::new();
    for _ in 0..6 {
        statuses.push(login_from(&server, "nobody", &wrong, "198.51.100.7").await.as_u16());
    }
    assert_eq!(statuses, [401, 401, 401, 401, 401, 429]);
    // One /64 is one client.
    for _ in 0..5 {
        login_from(&server, "alice", &wrong, "2001:db8:9:9::1").await;
    }
    assert_eq!(login_from(&server, "alice", &right, "2001:db8:9:9:ffff::2").await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(login_from(&server, "no such name!", &wrong, home).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn recovery_is_not_limited_per_account() {
    let server = TestServer::new();
    register(&server, "alice").await;
    // Others' wrong guesses do not use up alice's attempts (each address has its own five an hour).
    for i in 0..4 {
        let request = Request::post("/api/v1/auth/recover")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", format!("203.0.113.{i}"))
            .body(Body::from(json!({ "username": "alice", "recovery_auth_key": wrong_key() }).to_string()))
            .unwrap();
        assert_eq!(server.send(request).await.0, StatusCode::UNAUTHORIZED);
    }
    let huge = json!({ "username": "x".repeat(900_000), "recovery_auth_key": wrong_key() });
    assert_eq!(server.call(Method::POST, "/auth/recover", None, Some(huge)).await.0, StatusCode::UNAUTHORIZED);
}

fn wrong_key() -> String {
    memo_core::b64_encode([0u8; 32])
}

#[tokio::test]
async fn large_memos_can_be_revoked() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    let (bob, _) = register(&server, "bob").await;
    let id = alice.create(&"a".repeat(800_000));
    alice.sync(&server).await;
    alice.share(&server, id, "bob", Permission::Read).await;
    alice.sync(&server).await;
    assert_eq!(revoke(&server, &mut alice, id, &[bob.account.user_id]).await, StatusCode::OK);
    alice.sync(&server).await;
    assert_eq!(alice.memo(id).key_version, 2);
}

#[tokio::test]
async fn registration_refuses_unusable_public_keys() {
    let server = TestServer::new();
    let reg = memo_core::register(PASSWORD).unwrap();
    let mut request = json!({
        "invite_code": server.invite(), "user_id": reg.user_id, "username": "mallory",
        "kdf": KdfConfig { salt: reg.kdf_salt, params: reg.kdf }, "auth_key": reg.auth_key,
        "recovery_auth_key": reg.recovery_auth_key, "public_key": memo_core::b64_encode([0u8; 32]),
        "enc_account_key": memo_core::b64_encode(&reg.enc_account_key),
        "enc_account_key_recovery": memo_core::b64_encode(&reg.enc_account_key_recovery),
        "enc_private_key": memo_core::b64_encode(&reg.enc_private_key), "vault": memo_core::b64_encode(&reg.vault),
        "device": device(DeviceKind::Desktop),
    });
    let (status, body) = server.call(Method::POST, "/auth/register", None, Some(request.clone())).await;
    assert_eq!((status, body["error"]["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_request")));
    request["public_key"] = json!(reg.public_key);
    request["invite_code"] = json!("not an invite");
    let (status, body) = server.call(Method::POST, "/auth/register", None, Some(request)).await;
    assert_eq!((status, body["error"]["code"].as_str()), (StatusCode::FORBIDDEN, Some("invite_invalid")));
}

#[tokio::test]
async fn quota_counts_what_one_push_stores() {
    let server = TestServer::new();
    let (mut alice, _) = register(&server, "alice").await;
    // A short memo is 297 bytes of ciphertext: two fit, three do not.
    admin::user_set_quota(&server.app.db, "alice", Some(700)).unwrap();
    let ids = [alice.create("one"), alice.create("two"), alice.create("three")];
    let events = alice.sync(&server).await;
    let [SyncEvent::PushRejected { memo_id: rejected, reason: client::PushError::QuotaExceeded }] = events[..] else {
        panic!("{events:?}")
    };
    let used = |body: Value| body["used_bytes"].as_u64().unwrap();
    assert_eq!(used(server.call(Method::GET, "/me", Some(&alice.token), None).await.1), 2 * 297);
    // No room for a blob: refused before its body is read.
    let stored = *ids.iter().find(|id| **id != rejected).unwrap();
    let (status, ..) = alice.put_blob(&server, stored, Uuid::now_v7(), "original", vec![0; 500]).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // A delete in the same push makes room for what comes after it.
    client::delete_memo(&mut alice.store, &alice.account, stored).unwrap();
    let events = alice.sync(&server).await;
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(alice.memo(rejected).server_version, 1);
    assert_eq!(used(server.call(Method::GET, "/me", Some(&alice.token), None).await.1), 2 * 297);
}

#[cfg(unix)]
#[tokio::test]
async fn data_files_are_for_the_server_user_only() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let server = TestServer::new();
    register(&server, "alice").await;
    assert_eq!(mode(server.dir.clone()), 0o700);
    assert_eq!(mode(server.dir.join("memo.db")), 0o600);
    assert_eq!(mode(server.dir.join("memo.db-wal")), 0o600);
    let backup = std::env::temp_dir().join(format!("memo-backup-{}", Uuid::now_v7()));
    admin::backup(&server.app.db, &server.dir, &backup).unwrap();
    assert_eq!((mode(backup.clone()), mode(backup.join("memo.db"))), (0o700, 0o600));
    std::fs::remove_dir_all(backup).unwrap();
}

#[tokio::test]
async fn gc_drops_devices_revoked_long_ago() {
    let server = TestServer::new();
    let (alice, _) = register(&server, "alice").await;
    let second = login(&server, "alice", PASSWORD).await.unwrap();
    assert_eq!(server.call(Method::POST, "/auth/logout", Some(&second.token), None).await.0, StatusCode::NO_CONTENT);
    let devices = || {
        server
            .app
            .db
            .read_blocking(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM devices", [], |r| r.get::<_, i64>(0))?))
            .unwrap()
    };
    jobs::gc(&server.app.db, &server.app.config).unwrap();
    assert_eq!(devices(), 2, "kept while recent");
    jobs::gc_at(&server.app.db, &server.app.config, memo_server::db::now_ms() + 91 * 24 * 3600 * 1000).unwrap();
    assert_eq!(devices(), 1);
    assert_eq!(server.call(Method::GET, "/me", Some(&alice.token), None).await.0, StatusCode::OK);
}
