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
