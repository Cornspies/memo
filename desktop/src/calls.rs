//! What the frontend can ask of the Rust side (`web/src/lib/native.ts`). Each call takes and returns
//! bytes. Calls that carry JSON and data frame them as a big-endian `u32` length, the JSON, then the
//! data. Errors are strings; `network: …` means the server could not be reached.
//!
//! The device token and the account's keys stay on this side. A sign-in's token is taken out of
//! the server's answer and the keys out of memo-core's before the frontend sees them;
//! `session_commit` makes them the session and stores them. Requests then get the token added
//! here and only go to the server the account signed in to. So a script that got into the web
//! view could read the memos it shows, but not take the keys or the token with it.

use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use ureq::{
    Agent,
    http::Request,
    tls::{RootCerts, TlsConfig},
};

const CLIENT: &str = "desktop/0.1.0";
/// Larger than any response the API sends (attachments are at most 25 MB plus overhead).
const MAX_RESPONSE: u64 = 64 << 20;
const KEYRING_SERVICE: &str = "memo";
const KEYRING_USER: &str = "account";
/// The signed-in account without its secrets (server, username, user id), so a locked app knows it.
const ACCOUNT_FILE: &str = "account.json";

/// Where the account's secrets live: the OS keyring, or (in test mode) a file.
pub enum Secrets {
    Keyring,
    File(PathBuf),
}

pub struct Ctx {
    pub data_dir: PathBuf,
    pub downloads: PathBuf,
    pub secrets: Secrets,
    agent: Agent,
    /// The keyring store could not be opened: secrets are not kept, and the app asks for the
    /// password at every start.
    keyring_error: Mutex<Option<String>>,
    /// Set while signed in and unlocked.
    session: Mutex<Option<Session>>,
    /// What a sign-in in progress has produced so far.
    pending: Mutex<Pending>,
}

#[derive(Clone)]
struct Session {
    server: String,
    token: String,
}

#[derive(Default)]
struct Pending {
    server: Option<String>,
    token: Option<String>,
    /// The `secrets` memo-core returned when it opened the account.
    secrets: Option<Value>,
}

/// The API calls a signed-out app may make, to any server the user names.
const SIGN_IN_PATHS: [&str; 5] =
    ["/auth/prelogin", "/auth/login", "/auth/register", "/auth/recover", "/auth/recover/complete"];
/// Those of them that answer with a device token.
const TOKEN_PATHS: [&str; 3] = ["/auth/login", "/auth/register", "/auth/recover/complete"];
/// The memo-core calls that open an account and return its keys.
const OPENING_CALLS: [&str; 4] = ["register_finish", "login_finish", "recover_finish", "open"];

impl Ctx {
    pub fn new(data_dir: PathBuf, downloads: PathBuf, secrets: Secrets) -> Result<Arc<Ctx>, String> {
        fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        // Decrypted memos and their keys live below it.
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        let mut keyring_error = None;
        if matches!(secrets, Secrets::Keyring) {
            match dbus_secret_service_keyring_store::Store::new() {
                Ok(store) => keyring_core::set_default_store(store),
                Err(e) => keyring_error = Some(e.to_string()),
            }
        }
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(60)))
            .tls_config(TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build())
            .build()
            .into();
        let call =
            memo_native::call(json!({ "call": "init", "dir": data_dir.join("db") }).to_string().as_bytes(), None);
        if call[0] != 0 {
            return Err(String::from_utf8_lossy(&call[5..]).into_owned());
        }
        Ok(Arc::new(Ctx {
            data_dir,
            downloads,
            secrets,
            agent,
            keyring_error: Mutex::new(keyring_error),
            session: Mutex::default(),
            pending: Mutex::default(),
        }))
    }
}

pub fn dispatch(ctx: &Ctx, name: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    match name {
        "core" => {
            let (request, data) = split(body)?;
            let reply = memo_native::call(request, (!data.is_empty()).then_some(data));
            Ok(keep_secrets(ctx, request, reply))
        }
        "http" => http(ctx, body),
        "session_restore" => session_restore(ctx).map(|v| v.to_string().into_bytes()),
        "session_commit" => session_commit(ctx).map(|v| v.to_string().into_bytes()),
        "session_clear" => session_clear(ctx).map(|()| b"null".to_vec()),
        "account_get" => account_get(ctx),
        "account_set" => write_private(&ctx.data_dir.join(ACCOUNT_FILE), body).map(|()| b"null".to_vec()),
        "account_delete" => match fs::remove_file(ctx.data_dir.join(ACCOUNT_FILE)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(b"null".to_vec()),
        },
        "info" => Ok(info().to_string().into_bytes()),
        "save" => save(ctx, body).map(|path| json!(path).to_string().into_bytes()),
        "open" => open(std::str::from_utf8(body).map_err(|e| e.to_string())?).map(|()| b"null".to_vec()),
        _ => Err(format!("unknown call {name}")),
    }
}

fn split(body: &[u8]) -> Result<(&[u8], &[u8]), String> {
    let len = body.get(..4).ok_or("short request")?;
    let len = u32::from_be_bytes(len.try_into().expect("4 bytes")) as usize;
    let json = body.get(4..4 + len).ok_or("short request")?;
    Ok((json, &body[4 + len..]))
}

/// A reply of `memo_native::call`: its status byte, JSON and data.
fn reply_parts(reply: &[u8]) -> Option<(u8, Value, &[u8])> {
    let len = u32::from_be_bytes(reply.get(1..5)?.try_into().ok()?) as usize;
    let json = serde_json::from_slice(reply.get(5..5 + len)?).ok()?;
    Some((reply[0], json, &reply[5 + len..]))
}

/// When memo-core opened an account, its keys stay here and the frontend gets the rest.
fn keep_secrets(ctx: &Ctx, request: &[u8], reply: Vec<u8>) -> Vec<u8> {
    let call = serde_json::from_slice::<Value>(request).ok();
    let call = call.as_ref().and_then(|r| r["call"].as_str()).unwrap_or_default();
    if !OPENING_CALLS.contains(&call) {
        return reply;
    }
    let Some((0, mut opened, _)) = reply_parts(&reply) else { return reply };
    let Some(secrets) = opened.as_object_mut().and_then(|o| o.remove("secrets")) else { return reply };
    ctx.pending.lock().expect("lock").secrets = Some(secrets);
    let json = opened.to_string().into_bytes();
    let mut out = vec![0];
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out
}

#[derive(Deserialize)]
struct HttpRequest {
    method: String,
    /// Below `/api/v1`.
    path: String,
    /// For signing in. Once signed in, requests only go to the account's server.
    server: Option<String>,
    /// Add this device's token.
    auth: bool,
    #[serde(rename = "type")]
    content_type: Option<String>,
}

/// The API, from Rust: the webview's `fetch` would need CORS. Returns the status (big-endian `u32`)
/// and the body. Statuses are not errors; the frontend reads the API's error bodies.
fn http(ctx: &Ctx, body: &[u8]) -> Result<Vec<u8>, String> {
    let (request, data) = split(body)?;
    let r: HttpRequest = serde_json::from_slice(request).map_err(|e| e.to_string())?;
    let session = ctx.session.lock().expect("lock").clone();
    let (server, token) = match session {
        Some(session) => {
            if r.server.as_ref().is_some_and(|server| *server != session.server) {
                return Err("bad_url: signed in to another server".into());
            }
            (session.server, r.auth.then_some(session.token))
        }
        None if r.auth => return Err("locked: no session".into()),
        None if !SIGN_IN_PATHS.contains(&r.path.as_str()) => return Err("locked: not signed in".into()),
        None => (r.server.ok_or("bad_url: no server")?, None),
    };
    if !(server.starts_with("https://") || server.starts_with("http://")) || !r.path.starts_with('/') {
        return Err("bad_url: not an http(s) address".into());
    }
    let url = format!("{server}/api/v1{}", r.path);
    let mut builder = Request::builder().method(r.method.as_str()).uri(&url).header("Memo-Client", CLIENT);
    builder = builder.header("Accept", "application/json");
    if let Some(token) = &token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(content_type) = &r.content_type {
        builder = builder.header("Content-Type", content_type);
    }
    let request = builder.body(data.to_vec()).map_err(|e| format!("bad_url: {e}"))?;
    let mut response = ctx.agent.run(request).map_err(|e| format!("network: {e}"))?;
    let status = response.status().as_u16();
    let mut bytes =
        response.body_mut().with_config().limit(MAX_RESPONSE).read_to_vec().map_err(|e| format!("network: {e}"))?;
    // A new device token: it stays here, with the server it is for.
    if token.is_none() && (200..300).contains(&status) && TOKEN_PATHS.contains(&r.path.as_str()) {
        let mut answer: Value = serde_json::from_slice(&bytes).map_err(|_| "network: not a memo server")?;
        let device_token = answer.as_object_mut().and_then(|o| o.remove("token"));
        let Some(Value::String(device_token)) = device_token else { return Err("network: no device token".into()) };
        *ctx.pending.lock().expect("lock") = Pending { server: Some(server), token: Some(device_token), secrets: None };
        bytes = answer.to_string().into_bytes();
    }
    let mut out = u32::from(status).to_be_bytes().to_vec();
    out.extend_from_slice(&bytes);
    Ok(out)
}

/// Opens the account stored on this device. `{user_id, fingerprint}`, `null` if there is none, or
/// `{"unavailable": "…"}` when there is no keyring to read.
fn session_restore(ctx: &Ctx) -> Result<Value, String> {
    let stored = match secrets_get(ctx)? {
        Value::String(text) => text,
        other => return Ok(other),
    };
    let stored: Value = serde_json::from_str(&stored).unwrap_or_default();
    let (Some(token), Some(secrets)) = (stored["token"].as_str(), stored.get("secrets")) else {
        return Ok(Value::Null);
    };
    // Entries written before the server was stored with them: the account file has it.
    let server = match stored["server"].as_str() {
        Some(server) => Some(server.to_owned()),
        None => fs::read(ctx.data_dir.join(ACCOUNT_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|account| account["server"].as_str().map(str::to_owned)),
    };
    let Some(server) = server else { return Ok(Value::Null) };
    let reply = memo_native::call(json!({ "call": "open", "secrets": secrets }).to_string().as_bytes(), None);
    // Unreadable keys: unlock with the password.
    let Some((0, opened, _)) = reply_parts(&reply) else { return Ok(Value::Null) };
    if stored["server"].is_null() {
        let _ = secrets_set(ctx, &json!({ "server": server, "token": token, "secrets": secrets }).to_string());
    }
    *ctx.session.lock().expect("lock") = Some(Session { server, token: token.to_owned() });
    Ok(json!({ "user_id": opened["user_id"], "fingerprint": opened["fingerprint"] }))
}

/// The sign-in is complete: its token and keys become the session and go to the keyring.
/// `{"stored": false}` without a keyring: the keys stay in memory, and the next start asks for the
/// password.
fn session_commit(ctx: &Ctx) -> Result<Value, String> {
    let pending = std::mem::take(&mut *ctx.pending.lock().expect("lock"));
    let (Some(server), Some(token), Some(secrets)) = (pending.server, pending.token, pending.secrets) else {
        return Err("no sign-in to finish".into());
    };
    let stored = json!({ "server": server, "token": token, "secrets": secrets }).to_string();
    *ctx.session.lock().expect("lock") = Some(Session { server, token });
    Ok(json!({ "stored": secrets_set(ctx, &stored).is_ok() }))
}

/// Forgets the token and the stored keys (lock, sign-out).
fn session_clear(ctx: &Ctx) -> Result<(), String> {
    *ctx.session.lock().expect("lock") = None;
    *ctx.pending.lock().expect("lock") = Pending::default();
    secrets_delete(ctx)
}

/// The stored session (server, token, keys) as a JSON string, or `null`. `{"unavailable": "…"}`
/// when there is no keyring to read.
fn secrets_get(ctx: &Ctx) -> Result<Value, String> {
    match &ctx.secrets {
        Secrets::File(path) => match fs::read_to_string(path) {
            Ok(text) => Ok(Value::String(text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(e) => Err(e.to_string()),
        },
        Secrets::Keyring => {
            if let Some(error) = ctx.keyring_error.lock().expect("lock").clone() {
                return Ok(json!({ "unavailable": error }));
            }
            let entry = keyring_core::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())?;
            match entry.get_password() {
                Ok(text) => Ok(Value::String(text)),
                Err(keyring_core::Error::NoEntry) => Ok(Value::Null),
                Err(e) => Ok(json!({ "unavailable": e.to_string() })),
            }
        }
    }
}

fn secrets_set(ctx: &Ctx, value: &str) -> Result<(), String> {
    match &ctx.secrets {
        Secrets::File(path) => write_private(path, value.as_bytes()),
        Secrets::Keyring => {
            if ctx.keyring_error.lock().expect("lock").is_some() {
                return Err("keyring unavailable".into());
            }
            let entry = keyring_core::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())?;
            entry.set_password(value).map_err(|e| e.to_string())
        }
    }
}

fn secrets_delete(ctx: &Ctx) -> Result<(), String> {
    match &ctx.secrets {
        Secrets::File(path) => match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        },
        Secrets::Keyring => {
            if ctx.keyring_error.lock().expect("lock").is_some() {
                return Ok(());
            }
            let entry = keyring_core::Entry::new(KEYRING_SERVICE, KEYRING_USER).map_err(|e| e.to_string())?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut file, bytes).map_err(|e| e.to_string())?;
    fs::rename(tmp, path).map_err(|e| e.to_string())
}

/// The account file's JSON, or `null`.
fn account_get(ctx: &Ctx) -> Result<Vec<u8>, String> {
    match fs::read(ctx.data_dir.join(ACCOUNT_FILE)) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(b"null".to_vec()),
        Err(e) => Err(e.to_string()),
    }
}

/// The device name the server shows in the device list.
fn info() -> Value {
    let mut host = String::new();
    let _ = fs::File::open("/etc/hostname").and_then(|mut f| f.read_to_string(&mut host));
    let host = host.trim();
    json!({
        "device": if host.is_empty() { "Desktop".to_owned() } else { format!("Desktop ({host})") },
    })
}

#[derive(Deserialize)]
struct SaveRequest {
    name: String,
}

/// Writes a file into the downloads folder without overwriting anything. Returns its path.
fn save(ctx: &Ctx, body: &[u8]) -> Result<String, String> {
    let (request, data) = split(body)?;
    let r: SaveRequest = serde_json::from_slice(request).map_err(|e| e.to_string())?;
    let name = Path::new(&r.name).file_name().ok_or("bad file name")?.to_string_lossy().into_owned();
    fs::create_dir_all(&ctx.downloads).map_err(|e| e.to_string())?;
    let (stem, ext) = name.rsplit_once('.').unwrap_or((&name, ""));
    for i in 0..1000 {
        let candidate = match (i, ext) {
            (0, _) => name.clone(),
            (_, "") => format!("{stem} ({i})"),
            _ => format!("{stem} ({i}).{ext}"),
        };
        let path = ctx.downloads.join(candidate);
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                std::io::Write::write_all(&mut file, data).map_err(|e| e.to_string())?;
                return Ok(path.to_string_lossy().into_owned());
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("too many files with this name".into())
}

/// Opens a link from a memo in the default browser. Only the schemes `memo_core::render` keeps.
fn open(url: &str) -> Result<(), String> {
    let scheme = url.split_once(':').map(|(s, _)| s.to_ascii_lowercase()).unwrap_or_default();
    if !["http", "https", "mailto"].contains(&scheme.as_str()) || url.chars().any(char::is_control) {
        return Err("not a link this app opens".into());
    }
    Command::new("xdg-open").arg(url).spawn().map(drop).map_err(|e| e.to_string())
}
