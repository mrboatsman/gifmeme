//! The handful of Matrix C-S / Synapse admin endpoints gifmeme needs.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use urlencoding::encode;

use crate::config::Config;

pub const PACK_EVENT: &str = "im.ponies.room_emotes";

pub struct Matrix {
    http: reqwest::Client,
    base: String,
    token: String,
    pub user_id: String,
    pub server_name: String,
}

#[derive(Deserialize)]
struct MatrixError {
    errcode: Option<String>,
    error: Option<String>,
    retry_after_ms: Option<u64>,
}

/// Live view of a room from the bot's side.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RoomInfo {
    pub name: Option<String>,
    pub alias: Option<String>,
    pub bot_joined: bool,
    pub bot_power: i64,
    /// Power level needed to write im.ponies.room_emotes
    pub needed_power: i64,
    pub error: Option<String>,
}

impl RoomInfo {
    pub fn bot_can_write(&self) -> bool {
        self.bot_joined && self.bot_power >= self.needed_power
    }
}

/// Power levels of a room, enough to answer "what can user X do".
pub struct Powers {
    creators: Vec<String>,
    /// Room v12+: creators outrank everyone (they are not in `users`).
    creators_infinite: bool,
    users: serde_json::Map<String, Value>,
    users_default: i64,
    events: serde_json::Map<String, Value>,
    state_default: i64,
    has_power_levels: bool,
}

impl Powers {
    pub fn of(&self, user: &str) -> i64 {
        if self.creators.iter().any(|c| c == user) && (self.creators_infinite || !self.has_power_levels) {
            return if self.creators_infinite { i64::MAX } else { 100 };
        }
        self.users.get(user).and_then(Value::as_i64).unwrap_or(self.users_default)
    }

    pub fn needed_for_state(&self, event_type: &str) -> i64 {
        self.events.get(event_type).and_then(Value::as_i64).unwrap_or(self.state_default)
    }
}

impl Matrix {
    pub fn client(cfg: &Config) -> Result<reqwest::Client> {
        let mut b = reqwest::Client::builder()
            .user_agent(concat!("gifmeme/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120));
        if let Some(ip) = cfg.dns_server {
            b = b.dns_resolver(Arc::new(crate::dns::FixedDns::new(ip)?));
        }
        Ok(b.build()?)
    }

    /// whoami and lift the bot's rate limits.
    pub async fn connect(cfg: &Config) -> Result<Self> {
        let base = cfg.homeserver.clone().ok_or_else(|| anyhow!("MATRIX_HOMESERVER not set"))?;
        let token = cfg.access_token.clone().ok_or_else(|| anyhow!("MATRIX_ACCESS_TOKEN not set"))?;
        let mut m = Self { http: Self::client(cfg)?, base, token, user_id: String::new(), server_name: String::new() };

        let who = m.json(Method::GET, "/_matrix/client/v3/account/whoami", None).await?;
        m.user_id = who["user_id"].as_str().ok_or_else(|| anyhow!("whoami: no user_id"))?.to_string();
        m.server_name = m
            .user_id
            .split_once(':')
            .map(|(_, s)| s.to_string())
            .ok_or_else(|| anyhow!("odd user id {}", m.user_id))?;

        // Bursty pack rewrites trip rc_message; admins can exempt themselves.
        let path = format!("/_synapse/admin/v1/users/{}/override_ratelimit", encode(&m.user_id));
        match m.json(Method::POST, &path, Some(json!({ "messages_per_second": 0, "burst_count": 0 }))).await {
            Ok(_) => tracing::info!("rate limits lifted for {}", m.user_id),
            Err(e) => tracing::warn!("could not override rate limits (bot not a server admin?): {e:#}"),
        }
        Ok(m)
    }

    fn req(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("{}{}", self.base, path)).bearer_auth(&self.token)
    }

    /// Send with retries: 429 honours retry_after_ms, 5xx/network back off.
    async fn send(&self, build: impl Fn() -> RequestBuilder) -> Result<Value> {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let res = match build().send().await {
                Ok(r) => r,
                Err(e) if attempt < 4 => {
                    tracing::warn!("matrix request failed (attempt {attempt}): {e}");
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = res.status();
            let retry_header = res
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| s * 1000);
            let body = res.bytes().await.unwrap_or_default();
            if status.is_success() {
                return Ok(if body.is_empty() { json!({}) } else { serde_json::from_slice(&body)? });
            }
            let err: Option<MatrixError> = serde_json::from_slice(&body).ok();
            if status == StatusCode::TOO_MANY_REQUESTS && attempt < 8 {
                let wait = err.as_ref().and_then(|e| e.retry_after_ms).or(retry_header).unwrap_or(2000);
                tracing::info!("rate limited, retrying in {wait} ms");
                tokio::time::sleep(Duration::from_millis(wait.min(60_000))).await;
                continue;
            }
            if status.is_server_error() && attempt < 4 {
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                continue;
            }
            match err {
                Some(e) => bail!("{status}: {} {}", e.errcode.unwrap_or_default(), e.error.unwrap_or_default()),
                None => bail!("{status}: {}", String::from_utf8_lossy(&body)),
            }
        }
    }

    async fn json(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        self.send(|| {
            let r = self.req(method.clone(), path);
            match &body {
                Some(b) => r.json(b),
                None => r,
            }
        })
        .await
    }

    pub async fn upload(&self, filename: &str, mimetype: &str, bytes: bytes::Bytes) -> Result<String> {
        let path = format!("/_matrix/media/v3/upload?filename={}", encode(filename));
        let r = self
            .send(|| self.req(Method::POST, &path).header(reqwest::header::CONTENT_TYPE, mimetype).body(bytes.clone()))
            .await
            .context("media upload")?;
        r["content_uri"].as_str().map(str::to_string).ok_or_else(|| anyhow!("upload: no content_uri"))
    }

    /// Alias or id -> room id.
    pub async fn resolve_room(&self, room: &str) -> Result<String> {
        if room.starts_with('!') {
            return Ok(room.to_string());
        }
        if !room.starts_with('#') {
            bail!("expected a room id (!…) or alias (#…)");
        }
        let r = self
            .json(Method::GET, &format!("/_matrix/client/v3/directory/room/{}", encode(room)), None)
            .await
            .with_context(|| format!("resolving {room}"))?;
        r["room_id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("alias without room_id"))
    }

    /// Join (or accept an invite to) a room. Idempotent.
    pub async fn join(&self, room: &str) -> Result<String> {
        let r = self
            .json(Method::POST, &format!("/_matrix/client/v3/join/{}", encode(room)), Some(json!({})))
            .await?;
        r["room_id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("join: no room_id"))
    }

    async fn state_event(&self, room_id: &str, event_type: &str, key: &str) -> Result<Option<Value>> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{}/{}",
            encode(room_id),
            encode(event_type),
            encode(key)
        );
        match self.json(Method::GET, &path, None).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.to_string().starts_with("404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Needs the bot to be in the room (or the room to be world-readable).
    pub async fn powers(&self, room_id: &str) -> Result<Powers> {
        let path = format!("/_matrix/client/v3/rooms/{}/state", encode(room_id));
        let state = self.json(Method::GET, &path, None).await?;
        let events = state.as_array().cloned().unwrap_or_default();
        let find = |t: &str| events.iter().find(|e| e["type"] == t && e["state_key"] == "").map(|e| e.clone());
        let create = find("m.room.create").unwrap_or_default();
        let version = create["content"]["room_version"].as_str().unwrap_or("1").to_string();
        let mut creators = vec![create["sender"].as_str().unwrap_or_default().to_string()];
        if let Some(extra) = create["content"]["additional_creators"].as_array() {
            creators.extend(extra.iter().filter_map(|c| c.as_str().map(str::to_string)));
        }
        let creators_infinite = version.parse::<u32>().is_ok_and(|v| v >= 12) || version.contains("hydra");
        let pl = find("m.room.power_levels");
        let c = pl.as_ref().map(|e| e["content"].clone()).unwrap_or_default();
        let int = |v: &Value, d: i64| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(d);
        Ok(Powers {
            creators,
            creators_infinite,
            users: c["users"].as_object().cloned().unwrap_or_default(),
            users_default: int(&c["users_default"], 0),
            events: c["events"].as_object().cloned().unwrap_or_default(),
            state_default: int(&c["state_default"], if pl.is_some() { 50 } else { 0 }),
            has_power_levels: pl.is_some(),
        })
    }

    pub async fn membership(&self, room_id: &str, user_id: &str) -> Result<Option<String>> {
        Ok(self
            .state_event(room_id, "m.room.member", user_id)
            .await?
            .and_then(|m| m["membership"].as_str().map(str::to_string)))
    }

    /// Power level of a joined member; None when not joined.
    pub async fn member_power(&self, room_id: &str, user_id: &str) -> Result<Option<i64>> {
        if self.membership(room_id, user_id).await?.as_deref() != Some("join") {
            return Ok(None);
        }
        Ok(Some(self.powers(room_id).await?.of(user_id)))
    }

    pub async fn room_info(&self, room_id: &str) -> RoomInfo {
        let mut info = RoomInfo::default();
        let res: Result<()> = async {
            info.bot_joined = self.membership(room_id, &self.user_id).await?.as_deref() == Some("join");
            if !info.bot_joined {
                return Ok(());
            }
            let p = self.powers(room_id).await?;
            info.bot_power = p.of(&self.user_id);
            info.needed_power = p.needed_for_state(PACK_EVENT);
            info.name = self.state_event(room_id, "m.room.name", "").await?.and_then(|v| v["name"].as_str().map(str::to_string));
            info.alias = self
                .state_event(room_id, "m.room.canonical_alias", "")
                .await?
                .and_then(|v| v["alias"].as_str().map(str::to_string));
            Ok(())
        }
        .await;
        if let Err(e) = res {
            info.error = Some(format!("{e:#}"));
        }
        info
    }

    pub async fn put_pack(&self, room_id: &str, state_key: &str, content: &Value) -> Result<String> {
        let path = format!(
            "/_matrix/client/v3/rooms/{}/state/{}/{}",
            encode(room_id),
            PACK_EVENT,
            encode(state_key)
        );
        let r = self.json(Method::PUT, &path, Some(content.clone())).await?;
        Ok(r["event_id"].as_str().unwrap_or_default().to_string())
    }

    /// state_key -> content of every pack event currently in the room.
    pub async fn room_packs(&self, room_id: &str) -> Result<Vec<(String, Value)>> {
        let path = format!("/_matrix/client/v3/rooms/{}/state", encode(room_id));
        let r = self.json(Method::GET, &path, None).await?;
        Ok(r.as_array()
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e["type"] == PACK_EVENT)
                    .filter_map(|e| Some((e["state_key"].as_str()?.to_string(), e["content"].clone())))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Synapse admin API; Matrix itself never garbage-collects local media.
    pub async fn delete_media(&self, mxc: &str) -> Result<()> {
        let rest = mxc.strip_prefix("mxc://").ok_or_else(|| anyhow!("not an mxc uri: {mxc}"))?;
        let (server, media_id) = rest.split_once('/').ok_or_else(|| anyhow!("bad mxc uri: {mxc}"))?;
        if server != self.server_name {
            bail!("media {mxc} is not on this server");
        }
        let path = format!("/_synapse/admin/v1/media/{}/{}", encode(server), encode(media_id));
        match self.json(Method::DELETE, &path, None).await {
            Ok(_) => Ok(()),
            Err(e) if e.to_string().starts_with("404") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Identity provider ids offered by m.login.sso (e.g. "oidc-pocket_id").
    pub async fn sso_providers(&self) -> Result<Vec<String>> {
        let r = self.send(|| self.http.get(format!("{}/_matrix/client/v3/login", self.base))).await?;
        let flows = r["flows"].as_array().cloned().unwrap_or_default();
        let sso = flows
            .iter()
            .find(|f| f["type"] == "m.login.sso")
            .ok_or_else(|| anyhow!("homeserver offers no SSO login"))?;
        Ok(sso["identity_providers"]
            .as_array()
            .map(|ps| ps.iter().filter_map(|p| p["id"].as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    /// Exchange an SSO loginToken for the user id. The access token minted
    /// by the login is revoked right away; we only want to know who it is.
    pub async fn whoami_from_login_token(&self, login_token: &str) -> Result<String> {
        let body = json!({
            "type": "m.login.token",
            "token": login_token,
            "initial_device_display_name": "gif.lan login",
        });
        let res = self.http.post(format!("{}/_matrix/client/v3/login", self.base)).json(&body).send().await?;
        let status = res.status();
        let r: Value = res.json().await.unwrap_or_default();
        if !status.is_success() {
            bail!("{status}: {} {}", r["errcode"].as_str().unwrap_or(""), r["error"].as_str().unwrap_or(""));
        }
        let user_id = r["user_id"].as_str().ok_or_else(|| anyhow!("login: no user_id"))?.to_string();
        if let Some(token) = r["access_token"].as_str() {
            let logout = self
                .http
                .post(format!("{}/_matrix/client/v3/logout", self.base))
                .bearer_auth(token)
                .json(&json!({}))
                .send()
                .await;
            if let Err(e) = logout {
                tracing::warn!(%user_id, "could not revoke login token: {e}");
            }
        }
        Ok(user_id)
    }
}
