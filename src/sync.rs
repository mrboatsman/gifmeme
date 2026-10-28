//! Projects the DB into the configured rooms. Every change queues the
//! pack's slug; a single worker debounces the queue and rewrites
//! `im.ponies.room_emotes/<slug>` in full (idempotent) in every room the
//! category is published in, and blanks it (`{}`) in rooms it was published
//! in before but no longer is (`publications` remembers where). A full
//! resync runs at startup, after (re)connecting and on a schedule.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde_json::{Map, Value, json};
use tokio::sync::{RwLock, mpsc};

use crate::config::Config;
use crate::db::{self, Db};
use crate::matrix::Matrix;

const DEBOUNCE: Duration = Duration::from_millis(1500);
const MAX_DELAY: Duration = Duration::from_secs(10);
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Synapse rejects events over 65536 bytes (whole PDU); stay well below.
const MAX_CONTENT_BYTES: usize = 60_000;

#[derive(Debug)]
enum Msg {
    Pack(String),
    Full,
}

#[derive(Clone)]
pub struct Syncer {
    tx: mpsc::UnboundedSender<Msg>,
}

#[derive(Default, Clone, serde::Serialize)]
pub struct Status {
    pub connected: bool,
    pub user_id: Option<String>,
    pub error: Option<String>,
    pub last_full_sync: Option<i64>,
    pub last_full_sync_error: Option<String>,
}

pub type SharedMatrix = Arc<RwLock<Option<Arc<Matrix>>>>;
pub type SharedStatus = Arc<RwLock<Status>>;

impl Syncer {
    pub fn queue(&self, slug: &str) {
        let _ = self.tx.send(Msg::Pack(slug.to_string()));
    }

    pub fn full(&self) {
        let _ = self.tx.send(Msg::Full);
    }
}

pub fn start(cfg: Config, db: Db, matrix: SharedMatrix, status: SharedStatus) -> Syncer {
    let (tx, rx) = mpsc::unbounded_channel();
    let syncer = Syncer { tx };
    tokio::spawn(worker(cfg.clone(), db.clone(), matrix.clone(), status.clone(), syncer.clone(), rx));
    tokio::spawn(connector(cfg.clone(), db.clone(), matrix, status, syncer.clone()));
    let s = syncer.clone();
    let every = cfg.resync_interval;
    if !every.is_zero() {
        tokio::spawn(async move {
            let mut t = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
            loop {
                t.tick().await;
                s.full();
            }
        });
    }
    syncer
}

/// Keeps trying to reach the homeserver; a full resync follows each connect.
async fn connector(cfg: Config, db: Db, matrix: SharedMatrix, status: SharedStatus, syncer: Syncer) {
    if cfg.homeserver.is_none() || cfg.access_token.is_none() {
        tracing::warn!("MATRIX_HOMESERVER / MATRIX_ACCESS_TOKEN unset: running without Matrix");
        status.write().await.error = Some("Matrix is not configured (MATRIX_HOMESERVER/MATRIX_ACCESS_TOKEN)".into());
        return;
    }
    let mut delay = Duration::from_secs(5);
    loop {
        match Matrix::connect(&cfg).await {
            Ok(m) => {
                tracing::info!(user = %m.user_id, "connected to Matrix");
                if let Err(e) = seed_and_join(&cfg, &db, &m).await {
                    tracing::error!("room setup: {e:#}");
                }
                {
                    let mut s = status.write().await;
                    s.connected = true;
                    s.user_id = Some(m.user_id.clone());
                    s.error = None;
                }
                *matrix.write().await = Some(Arc::new(m));
                syncer.full();
                return;
            }
            Err(e) => {
                tracing::error!("Matrix connect failed, retrying in {delay:?}: {e:#}");
                status.write().await.error = Some(format!("{e:#}"));
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(300));
            }
        }
    }
}

/// First start after upgrading from the single-room version: turn
/// GIFMEME_ROOM into a configured room that grants logins to its members and
/// publishes every existing category. Then make sure the bot sits in every
/// configured room (accepts pending invites).
async fn seed_and_join(cfg: &Config, db: &Db, m: &Matrix) -> Result<()> {
    let rooms: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rooms").fetch_one(db).await?;
    if rooms == 0 {
        if let Some(seed) = &cfg.seed_room {
            let room_id = m.resolve_room(seed).await?;
            let alias = cfg.room_alias.clone().or_else(|| seed.starts_with('#').then(|| seed.clone()));
            tracing::info!(%room_id, "seeding room from GIFMEME_ROOM");
            sqlx::query(
                "INSERT INTO rooms (room_id, alias, login_min_power, default_publish, added_by, created_at)
                 VALUES (?, ?, 0, 1, 'GIFMEME_ROOM', ?)",
            )
            .bind(&room_id)
            .bind(alias)
            .bind(db::now())
            .execute(db)
            .await?;
            sqlx::query("INSERT OR IGNORE INTO category_rooms (category_id, room_id) SELECT id, ? FROM categories")
                .bind(&room_id)
                .execute(db)
                .await?;
            // The packs are already in that room from the single-room days.
            sqlx::query("INSERT OR IGNORE INTO publications (slug, room_id) SELECT slug, ? FROM categories")
                .bind(&room_id)
                .execute(db)
                .await?;
        }
    }
    let ids: Vec<String> = sqlx::query_scalar("SELECT room_id FROM rooms").fetch_all(db).await?;
    for id in ids {
        if m.membership(&id, &m.user_id).await.ok().flatten().as_deref() != Some("join") {
            match m.join(&id).await {
                Ok(_) => tracing::info!(room = %id, "joined"),
                Err(e) => tracing::warn!(room = %id, "bot is not in the room and cannot join (invite it): {e:#}"),
            }
        }
    }
    Ok(())
}

async fn worker(
    cfg: Config,
    db: Db,
    matrix: SharedMatrix,
    status: SharedStatus,
    syncer: Syncer,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    while let Some(first) = rx.recv().await {
        let mut slugs = BTreeSet::new();
        let mut full = false;
        let take = |m: Msg, slugs: &mut BTreeSet<String>, full: &mut bool| match m {
            Msg::Pack(s) => {
                slugs.insert(s);
            }
            Msg::Full => *full = true,
        };
        take(first, &mut slugs, &mut full);
        let deadline = tokio::time::Instant::now() + MAX_DELAY;
        loop {
            let wait = DEBOUNCE.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            match tokio::time::timeout(wait, rx.recv()).await {
                Ok(Some(m)) => take(m, &mut slugs, &mut full),
                Ok(None) => return,
                Err(_) => break,
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }

        let m = matrix.read().await.clone();
        let Some(m) = m else {
            // Not connected (yet): the connector triggers a full resync on
            // connect, which covers everything queued meanwhile.
            continue;
        };

        if full {
            match full_resync(&cfg, &db, &m).await {
                Ok(()) => {
                    let mut s = status.write().await;
                    s.last_full_sync = Some(db::now());
                    s.last_full_sync_error = None;
                }
                Err(e) => {
                    tracing::error!("full resync failed: {e:#}");
                    status.write().await.last_full_sync_error = Some(format!("{e:#}"));
                    retry_later(&syncer, None);
                }
            }
            continue;
        }
        for slug in slugs {
            if let Err(e) = sync_pack(&db, &m, &slug).await {
                tracing::error!(%slug, "pack sync failed: {e:#}");
                retry_later(&syncer, Some(slug));
            }
        }
    }
}

fn retry_later(syncer: &Syncer, slug: Option<String>) {
    let s = syncer.clone();
    tokio::spawn(async move {
        tokio::time::sleep(RETRY_AFTER).await;
        match slug {
            Some(slug) => s.queue(&slug),
            None => s.full(),
        }
    });
}

async fn full_resync(cfg: &Config, db: &Db, m: &Matrix) -> Result<()> {
    let cats = db::categories(db).await?;
    let mut failed = 0;
    for c in &cats {
        if let Err(e) = sync_pack(db, m, &c.slug).await {
            tracing::error!(slug = %c.slug, "pack sync failed: {e:#}");
            failed += 1;
        }
    }
    // Media whose pack is gone entirely (category deleted while offline).
    let orphan_slugs: Vec<String> = sqlx::query_scalar(
        "SELECT slug FROM media_trash WHERE slug NOT IN (SELECT slug FROM categories)
         UNION SELECT slug FROM publications WHERE slug NOT IN (SELECT slug FROM categories)",
    )
    .fetch_all(db)
    .await?;
    for slug in orphan_slugs {
        if let Err(e) = sync_pack(db, m, &slug).await {
            tracing::error!(%slug, "blanking deleted pack failed: {e:#}");
            failed += 1;
        }
    }
    if cfg.prune_unknown_packs {
        let rooms: Vec<String> = sqlx::query_scalar("SELECT room_id FROM rooms").fetch_all(db).await?;
        for room in rooms {
            let wanted: Vec<String> = sqlx::query_scalar(
                "SELECT c.slug FROM categories c JOIN category_rooms cr ON cr.category_id = c.id WHERE cr.room_id = ?",
            )
            .bind(&room)
            .fetch_all(db)
            .await?;
            for (key, content) in m.room_packs(&room).await? {
                let empty = content.as_object().is_none_or(|o| o.is_empty());
                if !empty && !wanted.contains(&key) {
                    tracing::warn!(%room, state_key = %key, "blanking pack not in the database");
                    m.put_pack(&room, &key, &json!({})).await?;
                }
            }
        }
    }
    if failed > 0 {
        bail!("{failed} pack(s) failed");
    }
    tracing::info!(packs = cats.len(), "full resync done");
    Ok(())
}

/// Upload missing media, write the pack to every room it belongs in, blank
/// it where it no longer belongs, then delete trashed media.
pub async fn sync_pack(db: &Db, m: &Matrix, slug: &str) -> Result<()> {
    let cat = db::category_by_slug(db, slug).await?;
    let content = match &cat {
        None => json!({}),
        Some(cat) => pack_content(db, m, cat).await?,
    };
    let size = serde_json::to_vec(&content)?.len();
    if size > MAX_CONTENT_BYTES {
        let err = format!("pack is {size} bytes, over the {MAX_CONTENT_BYTES} byte event limit");
        record_category(db, slug, Err(&err)).await?;
        bail!(err);
    }

    let desired: Vec<String> = match &cat {
        None => vec![],
        Some(c) => sqlx::query_scalar("SELECT room_id FROM category_rooms WHERE category_id = ?")
            .bind(c.id)
            .fetch_all(db)
            .await?,
    };
    let published: Vec<String> = sqlx::query_scalar("SELECT room_id FROM publications WHERE slug = ?")
        .bind(slug)
        .fetch_all(db)
        .await?;

    let mut errors = Vec::new();
    for room in &desired {
        match m.put_pack(room, slug, &content).await {
            Ok(event_id) => {
                tracing::info!(%slug, %room, %event_id, bytes = size, "pack synced");
                sqlx::query(
                    "INSERT INTO publications (slug, room_id, event_id, synced_at, error) VALUES (?, ?, ?, ?, NULL)
                     ON CONFLICT (slug, room_id) DO UPDATE SET event_id = excluded.event_id,
                       synced_at = excluded.synced_at, error = NULL",
                )
                .bind(slug)
                .bind(room)
                .bind(&event_id)
                .bind(db::now())
                .execute(db)
                .await?;
            }
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::error!(%slug, %room, "pack sync failed: {msg}");
                sqlx::query(
                    "INSERT INTO publications (slug, room_id, error) VALUES (?, ?, ?)
                     ON CONFLICT (slug, room_id) DO UPDATE SET error = excluded.error",
                )
                .bind(slug)
                .bind(room)
                .bind(&msg)
                .execute(db)
                .await?;
                errors.push(format!("{room}: {msg}"));
            }
        }
    }
    for room in published.iter().filter(|r| !desired.contains(r)) {
        match m.put_pack(room, slug, &json!({})).await {
            Ok(_) => tracing::info!(%slug, %room, "pack removed from room"),
            // Bot lost access to the room: nothing more we can do there.
            Err(e) if e.to_string().starts_with("403") || e.to_string().starts_with("404") => {
                tracing::warn!(%slug, %room, "cannot blank pack, giving up: {e:#}");
            }
            Err(e) => {
                errors.push(format!("{room}: {e:#}"));
                continue;
            }
        }
        sqlx::query("DELETE FROM publications WHERE slug = ? AND room_id = ?")
            .bind(slug)
            .bind(room)
            .execute(db)
            .await?;
    }

    if errors.is_empty() {
        record_category(db, slug, Ok(())).await?;
        empty_trash(db, m, slug).await;
        Ok(())
    } else {
        let err = errors.join("; ");
        record_category(db, slug, Err(&err)).await?;
        bail!(err)
    }
}

async fn record_category(db: &Db, slug: &str, res: Result<(), &str>) -> Result<()> {
    match res {
        Ok(()) => sqlx::query("UPDATE categories SET synced_at = ?, sync_error = NULL WHERE slug = ?")
            .bind(db::now())
            .bind(slug)
            .execute(db)
            .await?,
        Err(e) => sqlx::query("UPDATE categories SET sync_error = ? WHERE slug = ?")
            .bind(e)
            .bind(slug)
            .execute(db)
            .await?,
    };
    Ok(())
}

async fn pack_content(db: &Db, m: &Matrix, cat: &db::Category) -> Result<Value> {
    let stickers = db::stickers_in(db, cat.id).await?;
    let mut images = Map::new();
    for s in stickers {
        let mxc = match s.mxc_uri.clone() {
            Some(u) => u,
            None => upload_sticker(db, m, &s).await?,
        };
        images.insert(
            s.shortcode.clone(),
            json!({
                "url": mxc,
                "body": s.body.clone().filter(|b| !b.is_empty()).unwrap_or(s.shortcode.clone()),
                "usage": ["sticker"],
                "info": { "w": s.w, "h": s.h, "mimetype": s.mimetype, "size": s.size },
            }),
        );
    }
    if images.is_empty() {
        // Empty content = no pack; clients skip it.
        return Ok(json!({}));
    }
    let avatar = images.values().next().map(|i| i["url"].clone()).unwrap_or(Value::Null);
    Ok(json!({
        "pack": { "display_name": cat.display_name, "avatar_url": avatar, "usage": ["sticker"] },
        "images": images,
    }))
}

async fn upload_sticker(db: &Db, m: &Matrix, s: &db::Sticker) -> Result<String> {
    let path = crate::files_path(s.id);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|e| anyhow!("sticker {} has no file {}: {e}", s.id, path.display()))?;
    let mxc = m.upload(&s.filename, &s.mimetype, bytes.into()).await?;
    sqlx::query("UPDATE stickers SET mxc_uri = ? WHERE id = ?")
        .bind(&mxc)
        .bind(s.id)
        .execute(db)
        .await?;
    tracing::info!(id = s.id, %mxc, "uploaded");
    Ok(mxc)
}

async fn empty_trash(db: &Db, m: &Matrix, slug: &str) {
    let rows: Vec<(i64, String)> = match sqlx::query_as("SELECT id, mxc_uri FROM media_trash WHERE slug = ?")
        .bind(slug)
        .fetch_all(db)
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("reading media trash: {e}");
            return;
        }
    };
    for (id, mxc) in rows {
        // Still referenced elsewhere (e.g. moved, not deleted)? Keep it.
        let used: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stickers WHERE mxc_uri = ?")
            .bind(&mxc)
            .fetch_one(db)
            .await
            .unwrap_or(1);
        if used == 0 {
            if let Err(e) = m.delete_media(&mxc).await {
                tracing::warn!(%mxc, "media delete failed, will retry: {e:#}");
                continue;
            }
            tracing::info!(%mxc, "media deleted");
        }
        let _ = sqlx::query("DELETE FROM media_trash WHERE id = ?").bind(id).execute(db).await;
    }
}
