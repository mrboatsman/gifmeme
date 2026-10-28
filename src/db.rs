use std::str::FromStr;

use anyhow::Result;
use serde::Serialize;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{FromRow, SqlitePool};

pub type Db = SqlitePool;

pub async fn open(path: &std::path::Path) -> Result<Db> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(4).connect_with(opts).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Category {
    pub id: i64,
    pub slug: String,
    pub display_name: String,
    pub created_at: i64,
    pub synced_at: Option<i64>,
    pub event_id: Option<String>,
    pub sync_error: Option<String>,
}

#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Sticker {
    pub id: i64,
    pub category_id: i64,
    pub shortcode: String,
    pub body: Option<String>,
    pub filename: String,
    pub w: i64,
    pub h: i64,
    pub size: i64,
    pub mimetype: String,
    pub mxc_uri: Option<String>,
    #[serde(skip)]
    pub sha256: String,
    pub created_by: Option<String>,
    pub created_at: i64,
}

pub async fn category_by_slug(db: &Db, slug: &str) -> Result<Option<Category>> {
    Ok(sqlx::query_as::<_, Category>("SELECT * FROM categories WHERE slug = ?")
        .bind(slug)
        .fetch_optional(db)
        .await?)
}

pub async fn category_by_id(db: &Db, id: i64) -> Result<Option<Category>> {
    Ok(sqlx::query_as::<_, Category>("SELECT * FROM categories WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?)
}

pub async fn categories(db: &Db) -> Result<Vec<Category>> {
    Ok(sqlx::query_as::<_, Category>("SELECT * FROM categories ORDER BY display_name COLLATE NOCASE, id")
        .fetch_all(db)
        .await?)
}

pub async fn stickers_in(db: &Db, category_id: i64) -> Result<Vec<Sticker>> {
    Ok(sqlx::query_as::<_, Sticker>(
        "SELECT * FROM stickers WHERE category_id = ? ORDER BY created_at, id",
    )
    .bind(category_id)
    .fetch_all(db)
    .await?)
}

pub async fn sticker(db: &Db, id: i64) -> Result<Option<Sticker>> {
    Ok(sqlx::query_as::<_, Sticker>("SELECT * FROM stickers WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await?)
}

pub async fn count_in(db: &Db, category_id: i64) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM stickers WHERE category_id = ?")
        .bind(category_id)
        .fetch_one(db)
        .await?)
}

pub async fn shortcode_taken(db: &Db, category_id: i64, shortcode: &str, except: Option<i64>) -> Result<bool> {
    let n: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM stickers WHERE category_id = ? AND shortcode = ? AND id != ?",
    )
    .bind(category_id)
    .bind(shortcode)
    .bind(except.unwrap_or(-1))
    .fetch_one(db)
    .await?;
    Ok(n > 0)
}

/// `base`, or `base-2`, `base-3`, ... whichever is free in the category.
pub async fn unique_shortcode(db: &Db, category_id: i64, base: &str, except: Option<i64>) -> Result<String> {
    if !shortcode_taken(db, category_id, base, except).await? {
        return Ok(base.to_string());
    }
    for n in 2.. {
        let candidate = format!("{base}-{n}");
        if !shortcode_taken(db, category_id, &candidate, except).await? {
            return Ok(candidate);
        }
    }
    unreachable!()
}

pub async fn trash_media(db: &Db, slug: &str, mxc: &str) -> Result<()> {
    sqlx::query("INSERT INTO media_trash (slug, mxc_uri, created_at) VALUES (?, ?, ?)")
        .bind(slug)
        .bind(mxc)
        .bind(now())
        .execute(db)
        .await?;
    Ok(())
}
