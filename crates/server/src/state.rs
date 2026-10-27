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

pub struct AppState {
    pub config: Config,
    pub db: Arc<Db>,
    /// Derives fake prelogin salts.
    pub secret: [u8; 32],
    pub limiter: RateLimiter,
}

pub type App = Arc<AppState>;

impl AppState {
    pub fn open(config: Config) -> Result<App, String> {
        let dir = &config.data_dir;
        for sub in [dir.clone(), dir.join("blobs"), dir.join("tmp")] {
            fs::create_dir_all(&sub).map_err(|e| format!("{}: {e}", sub.display()))?;
        }
        let secret = load_or_create_secret(&dir.join("secret"))?;
        let db = Db::open(&db_path(dir)).map_err(|e| format!("opening database: {}", e.message))?;
        Ok(Arc::new(Self { config, db: Arc::new(db), secret, limiter: RateLimiter::default() }))
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
