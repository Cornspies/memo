//! Shared server state and the data directory layout (`docs/server.md#storage-layout`).

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use memo_core::AttachmentKind;
use uuid::Uuid;

use crate::{config::Config, db::Db, ratelimit::RateLimiter};

/// Argon2 runs at a time. Each takes 19 MiB; the rest wait.
const HASHING_SLOTS: usize = 8;

pub struct AppState {
    pub config: Config,
    pub db: Arc<Db>,
    /// Derives fake prelogin salts.
    pub secret: [u8; 32],
    pub limiter: RateLimiter,
    pub hashing: tokio::sync::Semaphore,
}

pub type App = Arc<AppState>;

impl AppState {
    pub fn open(config: Config) -> Result<App, String> {
        let dir = &config.data_dir;
        for sub in [dir.clone(), dir.join("blobs"), dir.join("tmp")] {
            create_private_dir(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
        }
        let secret = load_or_create_secret(&dir.join("secret"))?;
        let db = Db::open(&db_path(dir)).map_err(|e| format!("opening database: {}", e.message))?;
        Ok(Arc::new(Self {
            config,
            db: Arc::new(db),
            secret,
            limiter: RateLimiter::default(),
            hashing: tokio::sync::Semaphore::new(HASHING_SLOTS),
        }))
    }

    pub fn blob_path(&self, id: Uuid, kind: AttachmentKind) -> PathBuf {
        blob_path(&self.config.data_dir, id, kind)
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.config.data_dir.join("tmp")
    }
}

pub fn db_path(data_dir: &Path) -> PathBuf {
    data_dir.join("memo.db")
}

/// Creates the directory, and its missing parents, for the server's user only. One that exists
/// keeps its mode.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Creates an empty file for the server's user only, for SQLite to fill: its `-wal` and `-shm`
/// files take the database's mode. A file that exists is left alone.
pub fn create_private_file(path: &Path) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    match options.open(path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(e),
        _ => Ok(()),
    }
}

/// `blobs/<last 2 hex chars>/<id>.orig|.thumb`. The last characters, because UUIDv7 starts with
/// a timestamp.
pub fn blob_path(data_dir: &Path, id: Uuid, kind: AttachmentKind) -> PathBuf {
    let hex = id.simple().to_string();
    let ext = match kind {
        AttachmentKind::Original => "orig",
        AttachmentKind::Thumbnail => "thumb",
    };
    data_dir.join("blobs").join(&hex[30..]).join(format!("{hex}.{ext}"))
}

fn load_or_create_secret(path: &Path) -> Result<[u8; 32], String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    match fs::read(path) {
        Ok(bytes) => bytes.try_into().map_err(|_| format!("{}: expected 32 bytes", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let secret = crate::db::random_bytes::<32>();
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(path).map_err(err)?;
            file.write_all(&secret).and_then(|()| file.sync_all()).map_err(err)?;
            Ok(secret)
        }
        Err(e) => Err(err(e)),
    }
}
