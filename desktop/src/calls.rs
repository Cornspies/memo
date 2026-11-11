//! What the frontend can ask of the Rust side (`web/src/lib/native.ts`). Each call takes and returns
//! bytes. Calls that carry JSON and data frame them as a big-endian `u32` length, the JSON, then the
//! data. Errors are strings; `network: …` means the server could not be reached.

use std::{
    fs,
    io::Read,
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
}

impl Ctx {
    pub fn new(data_dir: PathBuf, downloads: PathBuf, secrets: Secrets) -> Result<Arc<Ctx>, String> {
        fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
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
        Ok(Arc::new(Ctx { data_dir, downloads, secrets, agent, keyring_error: Mutex::new(keyring_error) }))
    }
}

pub fn dispatch(ctx: &Ctx, name: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    match name {
        "core" => {
            let (request, data) = split(body)?;
            Ok(memo_native::call(request, (!data.is_empty()).then_some(data)))
        }
        "http" => http(ctx, body),
        "secrets_get" => secrets_get(ctx).map(|v| v.to_string().into_bytes()),
        "secrets_set" => {
            secrets_set(ctx, std::str::from_utf8(body).map_err(|e| e.to_string())?).map(|()| b"null".to_vec())
        }
        "secrets_delete" => secrets_delete(ctx).map(|()| b"null".to_vec()),
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

#[derive(Deserialize)]
struct HttpRequest {
    method: String,
    url: String,
    token: Option<String>,
    #[serde(rename = "type")]
    content_type: Option<String>,
}

/// The API, from Rust: the webview's `fetch` would need CORS. Returns the status (big-endian `u32`)
/// and the body. Statuses are not errors; the frontend reads the API's error bodies.
fn http(ctx: &Ctx, body: &[u8]) -> Result<Vec<u8>, String> {
    let (request, data) = split(body)?;
    let r: HttpRequest = serde_json::from_slice(request).map_err(|e| e.to_string())?;
    if !(r.url.starts_with("https://") || r.url.starts_with("http://")) {
        return Err("bad_url: not an http(s) address".into());
    }
    let mut builder = Request::builder().method(r.method.as_str()).uri(&r.url).header("Memo-Client", CLIENT);
    builder = builder.header("Accept", "application/json");
    if let Some(token) = &r.token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    if let Some(content_type) = &r.content_type {
        builder = builder.header("Content-Type", content_type);
    }
    let request = builder.body(data.to_vec()).map_err(|e| format!("bad_url: {e}"))?;
    let mut response = ctx.agent.run(request).map_err(|e| format!("network: {e}"))?;
    let status = response.status().as_u16() as u32;
    let bytes =
        response.body_mut().with_config().limit(MAX_RESPONSE).read_to_vec().map_err(|e| format!("network: {e}"))?;
    let mut out = status.to_be_bytes().to_vec();
    out.extend_from_slice(&bytes);
    Ok(out)
}

/// The stored account (server, username, token, keys) as JSON, or `null`. `{"unavailable": "…"}`
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
