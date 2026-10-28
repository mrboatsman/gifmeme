use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub data_dir: PathBuf,
    pub static_dir: PathBuf,
    /// Client-server API base, e.g. http://synapse.synapse.svc.cluster.local:8008
    pub homeserver: Option<String>,
    pub access_token: Option<String>,
    /// One-time seed: room id or alias configured on first start when the
    /// rooms table is empty (rooms are managed in the UI afterwards).
    pub seed_room: Option<String>,
    /// Shown in the UI and used for matrix.to links (defaults to `room`).
    pub room_alias: Option<String>,
    /// Resolve every hostname through this DNS server instead of
    /// /etc/resolv.conf (the NetBird sidecar replaces the pod resolver with
    /// one that NXDOMAINs *.svc.cluster.local).
    pub dns_server: Option<IpAddr>,
    pub pin: Option<String>,
    /// Browser-facing URL of this app (e.g. http://gif.lan). Enables
    /// "log in with Matrix" (SSO via the homeserver).
    pub public_url: Option<String>,
    /// Browser-facing homeserver URL for the SSO redirect (defaults to
    /// MATRIX_HOMESERVER, which is usually cluster-internal).
    pub matrix_public_url: Option<String>,
    pub session_days: i64,
    pub max_per_category: i64,
    pub max_px: u32,
    pub target_bytes: usize,
    pub max_upload_bytes: usize,
    pub resync_interval: Duration,
    pub prune_unknown_packs: bool,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn parse<T: std::str::FromStr>(name: &str, default: T) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    match var(name) {
        None => Ok(default),
        Some(v) => v
            .parse()
            .map_err(|e| anyhow::anyhow!("{name}={v:?}: {e}")),
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let access_token = match var("MATRIX_ACCESS_TOKEN_FILE") {
            Some(path) => Some(
                std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {path}"))?
                    .trim()
                    .to_string(),
            ),
            None => var("MATRIX_ACCESS_TOKEN"),
        };
        Ok(Self {
            listen: parse("GIFMEME_LISTEN", "0.0.0.0:8080".parse().unwrap())?,
            data_dir: parse("GIFMEME_DATA_DIR", PathBuf::from("./data"))?,
            static_dir: parse("GIFMEME_STATIC_DIR", PathBuf::from("./web/build"))?,
            homeserver: var("MATRIX_HOMESERVER").map(|s| s.trim_end_matches('/').to_string()),
            access_token,
            seed_room: var("GIFMEME_ROOM"),
            room_alias: var("GIFMEME_ROOM_ALIAS"),
            dns_server: match var("GIFMEME_DNS_SERVER") {
                Some(v) => Some(v.parse().context("GIFMEME_DNS_SERVER")?),
                None => None,
            },
            pin: var("GIFMEME_PIN"),
            public_url: var("GIFMEME_PUBLIC_URL").map(|s| s.trim_end_matches('/').to_string()),
            matrix_public_url: var("MATRIX_PUBLIC_URL").map(|s| s.trim_end_matches('/').to_string()),
            session_days: parse("GIFMEME_SESSION_DAYS", 30)?,
            max_per_category: parse("GIFMEME_MAX_PER_CATEGORY", 100)?,
            max_px: parse("GIFMEME_MAX_PX", 512)?,
            target_bytes: parse("GIFMEME_TARGET_BYTES", 500_000)?,
            max_upload_bytes: parse("GIFMEME_MAX_UPLOAD_MB", 50usize)? * 1024 * 1024,
            resync_interval: Duration::from_secs(
                parse("GIFMEME_RESYNC_INTERVAL_HOURS", 24u64)? * 3600,
            ),
            prune_unknown_packs: parse("GIFMEME_PRUNE_UNKNOWN_PACKS", false)?,
        })
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.data_dir.join("tmp")
    }
}
