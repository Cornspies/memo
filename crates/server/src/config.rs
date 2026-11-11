//! `config.toml` plus `MEMO_*` environment overrides. See `docs/server.md#configuration`.

use std::{
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Deserializer, de::Error as _};

pub const DEFAULT_PATH: &str = "/etc/memo/config.toml";

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub listen: SocketAddr,
    /// Origin of the web app (`https://memo.example.com`), without a trailing slash.
    pub public_url: String,
    pub data_dir: PathBuf,
    pub trusted_proxies: Vec<Cidr>,
    pub limits: Limits,
    pub retention: Retention,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub default_quota: Size,
    pub max_memo: Size,
    pub max_attachment: Size,
    pub max_thumbnail: Size,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    pub tombstone_days: u32,
    pub deleted_blob_days: u32,
    /// 0 keeps all.
    pub revisions_per_memo: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".parse().expect("valid address"),
            public_url: "http://localhost:8080".into(),
            data_dir: "/var/lib/memo".into(),
            trusted_proxies: vec![Cidr::parse("127.0.0.1/32").unwrap(), Cidr::parse("::1/128").unwrap()],
            limits: Limits::default(),
            retention: Retention::default(),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            default_quota: Size(1 << 30),
            max_memo: Size(1 << 20),
            max_attachment: Size(25 << 20),
            max_thumbnail: Size(1 << 20),
        }
    }
}

impl Default for Retention {
    fn default() -> Self {
        Self { tombstone_days: 90, deleted_blob_days: 30, revisions_per_memo: 100 }
    }
}

impl Config {
    /// Reads `path`, or [`DEFAULT_PATH`] if it exists, or uses the defaults; then applies the
    /// environment.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let path =
            path.map(Path::to_path_buf).or_else(|| Path::new(DEFAULT_PATH).exists().then(|| DEFAULT_PATH.into()));
        let mut config = match &path {
            Some(path) => {
                let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            None => Config::default(),
        };
        config.apply_env(|name| std::env::var(name).ok())?;
        config.validate()?;
        Ok(config)
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        fn parse<T: std::str::FromStr>(name: &str, value: &str) -> Result<T, String> {
            value.parse().map_err(|_| format!("{name}: invalid value {value:?}"))
        }
        if let Some(v) = var("MEMO_LISTEN") {
            self.listen = parse("MEMO_LISTEN", &v)?;
        }
        if let Some(v) = var("MEMO_PUBLIC_URL") {
            self.public_url = v;
        }
        if let Some(v) = var("MEMO_DATA_DIR") {
            self.data_dir = v.into();
        }
        if let Some(v) = var("MEMO_TRUSTED_PROXIES") {
            self.trusted_proxies =
                v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(Cidr::parse).collect::<Result<_, _>>()?;
        }
        if let Some(v) = var("MEMO_LIMITS_DEFAULT_QUOTA") {
            self.limits.default_quota = Size::parse(&v)?;
        }
        if let Some(v) = var("MEMO_LIMITS_MAX_MEMO") {
            self.limits.max_memo = Size::parse(&v)?;
        }
        if let Some(v) = var("MEMO_LIMITS_MAX_ATTACHMENT") {
            self.limits.max_attachment = Size::parse(&v)?;
        }
        if let Some(v) = var("MEMO_LIMITS_MAX_THUMBNAIL") {
            self.limits.max_thumbnail = Size::parse(&v)?;
        }
        if let Some(v) = var("MEMO_RETENTION_TOMBSTONE_DAYS") {
            self.retention.tombstone_days = parse("MEMO_RETENTION_TOMBSTONE_DAYS", &v)?;
        }
        if let Some(v) = var("MEMO_RETENTION_DELETED_BLOB_DAYS") {
            self.retention.deleted_blob_days = parse("MEMO_RETENTION_DELETED_BLOB_DAYS", &v)?;
        }
        if let Some(v) = var("MEMO_RETENTION_REVISIONS_PER_MEMO") {
            self.retention.revisions_per_memo = parse("MEMO_RETENTION_REVISIONS_PER_MEMO", &v)?;
        }
        Ok(())
    }

    fn validate(&mut self) -> Result<(), String> {
        let url = self.public_url.trim_end_matches('/').to_owned();
        let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
        if rest.is_none_or(|host| host.is_empty() || host.contains('/')) {
            return Err(format!("public_url must be an origin like https://memo.example.com, not {url:?}"));
        }
        self.public_url = url;
        if self.retention.deleted_blob_days > self.retention.tombstone_days {
            return Err("retention.deleted_blob_days must not exceed retention.tombstone_days".into());
        }
        Ok(())
    }

    /// Cookies get `Secure` unless the server is reached over plain http (local testing).
    pub fn secure_cookies(&self) -> bool {
        self.public_url.starts_with("https://")
    }

    /// Whether a browser `Origin` is this server's: the public URL, or for a loopback public URL
    /// any loopback name for it (`localhost`, `127.0.0.1`, `[::1]`) with the same scheme and port.
    pub fn origin_allowed(&self, origin: &str) -> bool {
        origin == self.public_url || loopback(origin).is_some_and(|o| Some(o) == loopback(&self.public_url))
    }
}

/// A loopback origin with its host written as `localhost`; `None` for any other origin.
fn loopback(origin: &str) -> Option<String> {
    let (scheme, rest) = origin.split_once("://")?;
    let (host, port) = match rest.find(']') {
        Some(end) if rest.starts_with('[') => rest.split_at(end + 1),
        _ => rest.rfind(':').map_or((rest, ""), |i| rest.split_at(i)),
    };
    matches!(host, "localhost" | "127.0.0.1" | "[::1]").then(|| format!("{scheme}://localhost{port}"))
}

/// A byte size: an integer, or a string like `"25MiB"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size(pub u64);

impl Size {
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
        let (number, unit) = text.split_at(split);
        let number: u64 = number.parse().map_err(|_| format!("invalid size {text:?}"))?;
        let factor: u64 = match unit.trim() {
            "" | "B" => 1,
            "KiB" => 1 << 10,
            "MiB" => 1 << 20,
            "GiB" => 1 << 30,
            "TiB" => 1 << 40,
            _ => return Err(format!("invalid size unit in {text:?} (use B, KiB, MiB, GiB or TiB)")),
        };
        number.checked_mul(factor).map(Size).ok_or_else(|| format!("size {text:?} is too large"))
    }
}

impl<'de> Deserialize<'de> for Size {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(u64),
            Text(String),
        }
        match Raw::deserialize(d)? {
            Raw::Number(n) => Ok(Size(n)),
            Raw::Text(text) => Size::parse(&text).map_err(D::Error::custom),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn parse(text: &str) -> Result<Self, String> {
        let invalid = || format!("invalid CIDR {text:?}");
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => {
                (addr.parse::<IpAddr>().map_err(|_| invalid())?, prefix.parse::<u8>().map_err(|_| invalid())?)
            }
            None => {
                let addr: IpAddr = text.parse().map_err(|_| invalid())?;
                (addr, if addr.is_ipv4() { 32 } else { 128 })
            }
        };
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(invalid());
        }
        Ok(Self { addr, prefix })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => mask_eq(u32::from(net).into(), u32::from(ip).into(), self.prefix, 32),
            (IpAddr::V6(net), IpAddr::V6(ip)) => mask_eq(u128::from(net), u128::from(ip), self.prefix, 128),
            _ => false,
        }
    }
}

fn mask_eq(a: u128, b: u128, prefix: u8, bits: u32) -> bool {
    if prefix == 0 {
        return true;
    }
    let shift = bits - u32::from(prefix);
    (a >> shift) == (b >> shift)
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Cidr::parse(&String::deserialize(d)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(Size::parse("1GiB").unwrap(), Size(1 << 30));
        assert_eq!(Size::parse("25 MiB").unwrap(), Size(25 << 20));
        assert_eq!(Size::parse("512").unwrap(), Size(512));
        assert!(Size::parse("1GB").is_err());
    }

    #[test]
    fn cidrs() {
        let net = Cidr::parse("10.0.0.0/8").unwrap();
        assert!(net.contains("10.1.2.3".parse().unwrap()));
        assert!(net.contains("::ffff:10.1.2.3".parse().unwrap()));
        assert!(!net.contains("11.0.0.1".parse().unwrap()));
        assert!(Cidr::parse("::1/128").unwrap().contains("::1".parse().unwrap()));
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains("1.2.3.4".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_err());
    }

    #[test]
    fn file_and_env() {
        let mut config: Config = toml::from_str(
            r#"
            public_url = "https://memo.example.com/"
            [limits]
            default_quota = "2GiB"
            [retention]
            revisions_per_memo = 0
            "#,
        )
        .unwrap();
        config
            .apply_env(|name| match name {
                "MEMO_LISTEN" => Some("0.0.0.0:9000".into()),
                "MEMO_LIMITS_MAX_MEMO" => Some("2MiB".into()),
                _ => None,
            })
            .unwrap();
        config.validate().unwrap();
        assert_eq!(config.public_url, "https://memo.example.com");
        assert_eq!(config.listen.port(), 9000);
        assert_eq!(config.limits.default_quota, Size(2 << 30));
        assert_eq!(config.limits.max_memo, Size(2 << 20));
        assert_eq!(config.retention.revisions_per_memo, 0);
        assert_eq!(config.retention.tombstone_days, 90);
        assert!(toml::from_str::<Config>("typo = 1").is_err());
    }

    #[test]
    fn origins() {
        let config = |url: &str| Config { public_url: url.into(), ..Config::default() };
        let public = config("https://memo.example.com");
        assert!(public.origin_allowed("https://memo.example.com"));
        assert!(!public.origin_allowed("http://memo.example.com"));
        assert!(!public.origin_allowed("https://localhost"));
        let local = config("http://127.0.0.1:8080");
        for origin in ["http://127.0.0.1:8080", "http://localhost:8080", "http://[::1]:8080"] {
            assert!(local.origin_allowed(origin), "{origin}");
        }
        for origin in
            ["http://localhost:8081", "https://localhost:8080", "http://localhost", "http://evil.example:8080"]
        {
            assert!(!local.origin_allowed(origin), "{origin}");
        }
        assert!(config("http://localhost").origin_allowed("http://127.0.0.1"));
    }
}
