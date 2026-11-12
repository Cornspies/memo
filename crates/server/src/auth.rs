//! Authentication: credential hashing, sessions (bearer token or cookie) and the `/auth` endpoints.

use std::{
    net::IpAddr,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use argon2::{Argon2, Params, PasswordHasher, PasswordVerifier};
use axum::{
    Extension, Json,
    extract::{FromRequestParts, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
use memo_core::{
    ENVELOPE_OVERHEAD, KdfParams,
    api::{
        DeviceInfo, DeviceKind, KdfConfig, LoginRequest, LoginResponse, PasswordChangeRequest, PreloginRequest,
        PreloginResponse, RecoverCompleteRequest, RecoverRequest, RecoverResponse, RegisterRequest, Role,
        SessionResponse,
    },
    b64_encode,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::Sha256;
use uuid::Uuid;

use crate::{
    db::{self, Entity, RowExt, now_ms, random_bytes, sha256},
    error::{ApiError, ApiResult},
    ratelimit::{HOUR, MINUTE, address_key},
    state::{App, AppState},
};

const COOKIE: &str = "memo_session";
/// Browsers cap cookie lifetimes at 400 days.
const COOKIE_MAX_AGE: u64 = 400 * 24 * 3600;
/// Wrapped account and private keys: an envelope around 32 bytes.
const ENC_KEY_LEN: usize = 32 + ENVELOPE_OVERHEAD;
const RECOVERY_TOKEN_MS: i64 = 10 * 60 * 1000;
const MAX_DEVICE_NAME: usize = 64;

/// The client address, set by the HTTP middleware.
#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

/// Filled in by [`Session`] so the request log can name the device.
#[derive(Clone, Default)]
pub struct LogDevice(pub Arc<Mutex<Option<Uuid>>>);

// ---- Credentials ----

/// `auth_key` and `recovery_auth_key` are 32 random-looking bytes, so a light Argon2id is plenty;
/// the slow part happens on the client.
fn hasher() -> Argon2<'static> {
    Argon2::from(Params::new(19456, 2, 1, Some(32)).expect("valid Argon2 parameters"))
}

pub fn hash_credential(secret: &[u8]) -> String {
    hasher().hash_password_with_salt(secret, &random_bytes::<16>()).expect("Argon2 hashing").to_string()
}

pub fn verify_credential(secret: &[u8], phc: &str) -> bool {
    hasher().verify_password(secret, phc).is_ok()
}

/// Spends the same time as a real check when there is nothing to check against.
fn verify_dummy(secret: &[u8]) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    verify_credential(secret, DUMMY.get_or_init(|| hash_credential(&[0; 32])));
}

/// Runs Argon2 off the async threads, a few at a time (`AppState::hashing`).
async fn hashing<R: Send + 'static>(app: &AppState, f: impl FnOnce() -> R + Send + 'static) -> ApiResult<R> {
    let _slot = app.hashing.acquire().await.map_err(ApiError::internal)?;
    tokio::task::spawn_blocking(f).await.map_err(ApiError::internal)
}

fn invite_invalid() -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, memo_core::api::ErrorCode::InviteInvalid, "invalid invite")
}

// ---- Sessions ----

#[derive(Clone, Debug)]
pub struct Session {
    pub user_id: Uuid,
    pub device_id: Uuid,
    pub kind: DeviceKind,
    pub role: Role,
}

impl FromRequestParts<App> for Session {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &App) -> ApiResult<Self> {
        let (token, via_cookie) = match bearer(&parts.headers) {
            Some(token) => (token, false),
            None => (cookie(&parts.headers).ok_or_else(ApiError::unauthenticated)?, true),
        };
        // CSRF: a cookie is sent by the browser on its own, so require the page's own origin.
        if via_cookie && !matches!(parts.method, Method::GET | Method::HEAD) {
            let origin = parts.headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
            if !origin.is_some_and(|o| app.config.origin_allowed(o)) {
                return Err(ApiError::forbidden("origin not allowed"));
            }
        }
        let token_hash = sha256(token.as_bytes());
        let found = app
            .db
            .read(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT d.id, d.user_id, d.kind, d.last_seen_at, u.role FROM devices d JOIN users u ON u.id = d.user_id
                         WHERE d.token_hash = ?1 AND d.revoked_at IS NULL AND u.disabled = 0",
                        [&token_hash],
                        |r| Ok((r.uuid(0)?, r.uuid(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get::<_, String>(4)?)),
                    )
                    .optional()?)
            })
            .await?;
        let (device_id, user_id, kind, last_seen, role) = found.ok_or_else(ApiError::unauthenticated)?;
        if let Some(log) = parts.extensions.get::<LogDevice>() {
            *log.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(device_id);
        }
        app.limiter.check("user", user_id.to_string(), 600, MINUTE)?;
        let now = now_ms();
        if now - last_seen > 60_000 {
            app.db
                .write(move |tx| {
                    tx.execute(
                        "UPDATE devices SET last_seen_at = ?1 WHERE id = ?2",
                        params![now, device_id.as_bytes()],
                    )?;
                    Ok(())
                })
                .await?;
        }
        Ok(Session {
            user_id,
            device_id,
            kind: parse_kind(&kind).ok_or_else(|| ApiError::internal("device kind"))?,
            role: if role == "admin" { Role::Admin } else { Role::User },
        })
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|t| t.trim().to_owned())
}

fn cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE).and_then(|rest| rest.strip_prefix('=')))
        .map(str::to_owned)
}

fn session_cookie(app: &AppState, token: &str, max_age: u64) -> HeaderValue {
    let secure = if app.config.secure_cookies() { "; Secure" } else { "" };
    let value = format!("{COOKIE}={token}; HttpOnly{secure}; SameSite=Strict; Path=/api; Max-Age={max_age}");
    HeaderValue::from_str(&value).expect("cookie is ASCII")
}

pub fn kind_str(kind: DeviceKind) -> &'static str {
    match kind {
        DeviceKind::Web => "web",
        DeviceKind::Desktop => "desktop",
        DeviceKind::Android => "android",
    }
}

pub fn parse_kind(text: &str) -> Option<DeviceKind> {
    Some(match text {
        "web" => DeviceKind::Web,
        "desktop" => DeviceKind::Desktop,
        "android" => DeviceKind::Android,
        _ => return None,
    })
}

/// A new device and its token. Web devices get the token as a cookie instead.
struct NewDevice {
    id: Uuid,
    token: String,
    kind: DeviceKind,
}

fn create_device(tx: &Transaction, user_id: Uuid, device: &DeviceInfo) -> ApiResult<NewDevice> {
    let id = Uuid::now_v7();
    let token = b64_encode(random_bytes::<32>());
    let now = now_ms();
    tx.execute(
        "INSERT INTO devices (id, user_id, name, kind, token_hash, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id.as_bytes(), user_id.as_bytes(), device.name.trim(), kind_str(device.kind), sha256(token.as_bytes()), now],
    )?;
    Ok(NewDevice { id, token, kind: device.kind })
}

impl NewDevice {
    /// The token in the body for native clients, in a cookie for the web.
    fn respond(
        self,
        app: &AppState,
        status: StatusCode,
        body: impl FnOnce(Option<String>) -> serde_json::Value,
    ) -> Response {
        if self.kind == DeviceKind::Web {
            let mut response = (status, Json(body(None))).into_response();
            response.headers_mut().insert(header::SET_COOKIE, session_cookie(app, &self.token, COOKIE_MAX_AGE));
            response
        } else {
            (status, Json(body(Some(self.token)))).into_response()
        }
    }
}

// ---- Validation ----

pub fn normalize_username(username: &str) -> ApiResult<String> {
    let username = username.trim().to_lowercase();
    let valid_chars = username.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c));
    if !(3..=32).contains(&username.len()) || !valid_chars {
        return Err(ApiError::bad_request("username must be 3-32 characters of a-z 0-9 _ . -"));
    }
    Ok(username)
}

fn check_device(device: &DeviceInfo) -> ApiResult<()> {
    let name = device.name.trim();
    if name.is_empty() || name.chars().count() > MAX_DEVICE_NAME {
        return Err(ApiError::bad_request("device name must be 1-64 characters"));
    }
    Ok(())
}

fn check_kdf(kdf: &KdfConfig) -> ApiResult<()> {
    kdf.params.check_bounds().map_err(|_| ApiError::bad_request("KDF parameters out of bounds"))
}

fn check_wrapped_key(bytes: &[u8], what: &str) -> ApiResult<()> {
    if bytes.len() != ENC_KEY_LEN {
        return Err(ApiError::bad_request(format!("{what} has the wrong length")));
    }
    Ok(())
}

// ---- Endpoints ----

pub async fn prelogin(
    State(app): State<App>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(request): Json<PreloginRequest>,
) -> ApiResult<Json<PreloginResponse>> {
    app.limiter.check("prelogin", address_key(ip), 20, MINUTE)?;
    let username = request.username.trim().to_lowercase();
    let lookup = username.clone();
    let found = app
        .db
        .read(move |conn| {
            Ok(conn
                .query_row("SELECT kdf_salt, kdf_m, kdf_t, kdf_p FROM users WHERE username = ?1", [&lookup], |r| {
                    Ok((r.get::<_, [u8; 16]>(0)?, KdfParams { m: r.get(1)?, t: r.get(2)?, p: r.get(3)? }))
                })
                .optional()?)
        })
        .await?;
    let (salt, params) = found.unwrap_or_else(|| {
        // Unknown users get a stable fake salt, so the answer does not reveal who exists.
        let mut mac = Hmac::<Sha256>::new_from_slice(&app.secret).expect("HMAC takes any key length");
        mac.update(b"memo/prelogin\0");
        mac.update(username.as_bytes());
        let digest = mac.finalize().into_bytes();
        (digest[..16].try_into().expect("16 bytes"), KdfParams::DEFAULT)
    });
    Ok(Json(PreloginResponse { kdf: KdfConfig { salt, params } }))
}

pub async fn register(
    State(app): State<App>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(request): Json<RegisterRequest>,
) -> ApiResult<Response> {
    app.limiter.check("register", address_key(ip), 5, HOUR)?;
    let username = normalize_username(&request.username)?;
    check_device(&request.device)?;
    check_kdf(&request.kdf)?;
    if request.user_id.get_version_num() != 7 {
        return Err(ApiError::bad_request("user_id must be a UUIDv7"));
    }
    if !request.public_key.is_usable() {
        return Err(ApiError::bad_request("public_key is not a key anything can be wrapped to"));
    }
    check_wrapped_key(&request.enc_account_key, "enc_account_key")?;
    check_wrapped_key(&request.enc_account_key_recovery, "enc_account_key_recovery")?;
    check_wrapped_key(&request.enc_private_key, "enc_private_key")?;
    if request.vault.len() > app.config.limits.max_memo.0 as usize {
        return Err(ApiError::too_large());
    }
    // An invite that does not work is not worth two Argon2 runs. The transaction checks it again.
    let invite_hash = sha256(request.invite_code.trim().as_bytes());
    let usable = app
        .db
        .read(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM invites WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2)",
                params![invite_hash, now_ms()],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .await?;
    if !usable {
        return Err(invite_invalid());
    }
    let auth_key = *request.auth_key.as_bytes();
    let recovery_key = *request.recovery_auth_key.as_bytes();
    let (auth_hash, recovery_hash) =
        hashing(&app, move || (hash_credential(&auth_key), hash_credential(&recovery_key))).await?;

    let device = app
        .db
        .write(move |tx| {
            let role: String = tx
                .query_row(
                    "SELECT role FROM invites WHERE code_hash = ?1 AND used_at IS NULL AND expires_at > ?2",
                    params![invite_hash, now_ms()],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or_else(invite_invalid)?;
            let taken: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM users WHERE username = ?1 OR id = ?2)",
                params![username, request.user_id.as_bytes()],
                |r| r.get(0),
            )?;
            if taken {
                return Err(ApiError::conflict("username or user id taken"));
            }
            let now = now_ms();
            let user_id = request.user_id;
            let kdf = request.kdf;
            tx.execute(
                "INSERT INTO users (id, username, role, auth_hash, kdf_salt, kdf_m, kdf_t, kdf_p, recovery_hash, public_key,
                                    enc_account_key, enc_account_key_recovery, enc_private_key, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    user_id.as_bytes(),
                    username,
                    role,
                    auth_hash,
                    kdf.salt,
                    kdf.params.m,
                    kdf.params.t,
                    kdf.params.p,
                    recovery_hash,
                    request.public_key.as_bytes(),
                    request.enc_account_key,
                    request.enc_account_key_recovery,
                    request.enc_private_key,
                    now
                ],
            )?;
            tx.execute(
                "INSERT INTO vaults (user_id, version, ciphertext, updated_at) VALUES (?1, 1, ?2, ?3)",
                params![user_id.as_bytes(), request.vault, now],
            )?;
            tx.execute("UPDATE invites SET used_by = ?1, used_at = ?2 WHERE code_hash = ?3", params![user_id.as_bytes(), now, invite_hash])?;
            db::log_change(tx, user_id, Entity::Vault, user_id)?;
            create_device(tx, user_id, &request.device)
        })
        .await?;
    let device_id = device.id;
    Ok(device.respond(&app, StatusCode::CREATED, |token| serde_json::json!(SessionResponse { device_id, token })))
}

/// A login that carries a valid session of the same user (the web client unlocking after a reload)
/// replaces that device instead of adding one.
pub async fn login(
    State(app): State<App>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> ApiResult<Response> {
    app.limiter.check("login", address_key(ip), 10, MINUTE)?;
    check_device(&request.device)?;
    // No account has a name like this.
    let username = normalize_username(&request.username).map_err(|_| ApiError::invalid_credentials())?;
    // Counted per name and address, for names that do not exist too (`docs/api.md#rate-limits`).
    let attempt = format!("{username} {}", address_key(ip));
    app.limiter.check_login(&attempt)?;
    let user = app
        .db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT id, auth_hash, public_key, enc_account_key, enc_private_key FROM users
                     WHERE username = ?1 AND disabled = 0",
                    [&username],
                    |r| {
                        Ok((
                            r.uuid(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, [u8; 32]>(2)?,
                            r.get::<_, Vec<u8>>(3)?,
                            r.get::<_, Vec<u8>>(4)?,
                        ))
                    },
                )
                .optional()?)
        })
        .await?;
    let auth_key = *request.auth_key.as_bytes();
    let Some((user_id, auth_hash, public_key, enc_account_key, enc_private_key)) = user else {
        hashing(&app, move || verify_dummy(&auth_key)).await?;
        app.limiter.login_failed(&attempt);
        return Err(ApiError::invalid_credentials());
    };
    if !hashing(&app, move || verify_credential(&auth_key, &auth_hash)).await? {
        app.limiter.login_failed(&attempt);
        return Err(ApiError::invalid_credentials());
    }
    app.limiter.login_succeeded(&attempt);
    let previous = bearer(&headers).or_else(|| cookie(&headers)).map(|token| sha256(token.as_bytes()));
    let device = app
        .db
        .write(move |tx| {
            if let Some(token_hash) = previous {
                tx.execute(
                    "UPDATE devices SET revoked_at = ?1 WHERE token_hash = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                    params![now_ms(), token_hash, user_id.as_bytes()],
                )?;
            }
            create_device(tx, user_id, &request.device)
        })
        .await?;
    let device_id = device.id;
    let public_key = memo_core::PublicKey::from_bytes(&public_key).map_err(ApiError::internal)?;
    Ok(device.respond(&app, StatusCode::OK, |token| {
        serde_json::json!(LoginResponse { user_id, device_id, token, public_key, enc_account_key, enc_private_key })
    }))
}

pub async fn logout(State(app): State<App>, session: Session) -> ApiResult<Response> {
    let device_id = session.device_id;
    app.db
        .write(move |tx| {
            tx.execute("UPDATE devices SET revoked_at = ?1 WHERE id = ?2", params![now_ms(), device_id.as_bytes()])?;
            Ok(())
        })
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if session.kind == DeviceKind::Web {
        response.headers_mut().insert(header::SET_COOKIE, session_cookie(&app, "", 0));
    }
    Ok(response)
}

fn current_auth_hash(conn: &Connection, user_id: Uuid) -> ApiResult<String> {
    Ok(conn.query_row("SELECT auth_hash FROM users WHERE id = ?1", [user_id.as_bytes()], |r| r.get(0))?)
}

/// Checks the caller's current `auth_key` (password change, account deletion). Its backoff is
/// apart from the login's: a stolen session cannot keep the owner from signing in.
pub async fn verify_current_auth_key(app: &App, user_id: Uuid, auth_key: [u8; 32]) -> ApiResult<()> {
    let attempt = format!("account {user_id}");
    app.limiter.check_login(&attempt)?;
    let hash = app.db.read(move |conn| current_auth_hash(conn, user_id)).await?;
    if !hashing(app, move || verify_credential(&auth_key, &hash)).await? {
        app.limiter.login_failed(&attempt);
        return Err(ApiError::invalid_credentials());
    }
    app.limiter.login_succeeded(&attempt);
    Ok(())
}

pub async fn change_password(
    State(app): State<App>,
    session: Session,
    Json(request): Json<PasswordChangeRequest>,
) -> ApiResult<StatusCode> {
    verify_current_auth_key(&app, session.user_id, *request.auth_key.as_bytes()).await?;
    check_kdf(&request.new.kdf)?;
    check_wrapped_key(&request.new.enc_account_key, "enc_account_key")?;
    let new_key = *request.new.auth_key.as_bytes();
    let auth_hash = hashing(&app, move || hash_credential(&new_key)).await?;
    app.db
        .write(move |tx| {
            let kdf = request.new.kdf;
            tx.execute(
                "UPDATE users SET auth_hash = ?1, kdf_salt = ?2, kdf_m = ?3, kdf_t = ?4, kdf_p = ?5, enc_account_key = ?6
                 WHERE id = ?7",
                params![
                    auth_hash,
                    kdf.salt,
                    kdf.params.m,
                    kdf.params.t,
                    kdf.params.p,
                    request.new.enc_account_key,
                    session.user_id.as_bytes()
                ],
            )?;
            if request.revoke_other_devices {
                tx.execute(
                    "UPDATE devices SET revoked_at = ?1 WHERE user_id = ?2 AND id != ?3 AND revoked_at IS NULL",
                    params![now_ms(), session.user_id.as_bytes(), session.device_id.as_bytes()],
                )?;
            }
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn recover(
    State(app): State<App>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(request): Json<RecoverRequest>,
) -> ApiResult<Json<RecoverResponse>> {
    app.limiter.check("recover", address_key(ip), 5, HOUR)?;
    let username = request.username.trim().to_lowercase();
    let user = app
        .db
        .read(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT id, recovery_hash, enc_account_key_recovery, public_key, enc_private_key FROM users
                     WHERE username = ?1 AND disabled = 0",
                    [&username],
                    |r| {
                        Ok((
                            r.uuid(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, Vec<u8>>(2)?,
                            r.get::<_, [u8; 32]>(3)?,
                            r.get::<_, Vec<u8>>(4)?,
                        ))
                    },
                )
                .optional()?)
        })
        .await?;
    let key = *request.recovery_auth_key.as_bytes();
    let Some((user_id, recovery_hash, enc_account_key_recovery, public_key, enc_private_key)) = user else {
        hashing(&app, move || verify_dummy(&key)).await?;
        return Err(ApiError::invalid_credentials());
    };
    if !hashing(&app, move || verify_credential(&key, &recovery_hash)).await? {
        return Err(ApiError::invalid_credentials());
    }
    let token = b64_encode(random_bytes::<32>());
    let token_hash = sha256(token.as_bytes());
    app.db
        .write(move |tx| {
            tx.execute(
                "INSERT INTO recovery_tokens (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
                params![token_hash, user_id.as_bytes(), now_ms() + RECOVERY_TOKEN_MS],
            )?;
            Ok(())
        })
        .await?;
    let public_key = memo_core::PublicKey::from_bytes(&public_key).map_err(ApiError::internal)?;
    Ok(Json(RecoverResponse { user_id, recovery_token: token, enc_account_key_recovery, public_key, enc_private_key }))
}

pub async fn recover_complete(
    State(app): State<App>,
    Extension(ClientIp(ip)): Extension<ClientIp>,
    Json(request): Json<RecoverCompleteRequest>,
) -> ApiResult<Response> {
    app.limiter.check("recover", address_key(ip), 5, HOUR)?;
    check_device(&request.device)?;
    check_kdf(&request.kdf)?;
    check_wrapped_key(&request.enc_account_key, "enc_account_key")?;
    check_wrapped_key(&request.enc_account_key_recovery, "enc_account_key_recovery")?;
    let (auth_key, recovery_key) = (*request.auth_key.as_bytes(), *request.recovery_auth_key.as_bytes());
    let (auth_hash, recovery_hash) =
        hashing(&app, move || (hash_credential(&auth_key), hash_credential(&recovery_key))).await?;
    let device = app
        .db
        .write(move |tx| {
            let token_hash = sha256(request.recovery_token.as_bytes());
            let user_id = tx
                .query_row(
                    "SELECT user_id FROM recovery_tokens WHERE token_hash = ?1 AND expires_at > ?2",
                    params![token_hash, now_ms()],
                    |r| r.uuid(0),
                )
                .optional()?
                .ok_or_else(ApiError::invalid_credentials)?;
            let kdf = request.kdf;
            tx.execute(
                "UPDATE users SET auth_hash = ?1, kdf_salt = ?2, kdf_m = ?3, kdf_t = ?4, kdf_p = ?5, enc_account_key = ?6,
                                  recovery_hash = ?7, enc_account_key_recovery = ?8
                 WHERE id = ?9",
                params![
                    auth_hash,
                    kdf.salt,
                    kdf.params.m,
                    kdf.params.t,
                    kdf.params.p,
                    request.enc_account_key,
                    recovery_hash,
                    request.enc_account_key_recovery,
                    user_id.as_bytes()
                ],
            )?;
            tx.execute(
                "UPDATE devices SET revoked_at = ?1 WHERE user_id = ?2 AND revoked_at IS NULL",
                params![now_ms(), user_id.as_bytes()],
            )?;
            tx.execute("DELETE FROM recovery_tokens WHERE user_id = ?1", [user_id.as_bytes()])?;
            create_device(tx, user_id, &request.device)
        })
        .await?;
    let device_id = device.id;
    Ok(device.respond(&app, StatusCode::OK, |token| serde_json::json!(SessionResponse { device_id, token })))
}
