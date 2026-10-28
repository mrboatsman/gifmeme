use std::sync::Arc;

use axum::extract::{Extension, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::{get, patch, post, put}};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, Semaphore};

use crate::access::Access;
use crate::auth;
use crate::config::Config;
use crate::db::{self, Category, Db, Sticker};
use crate::slug::{shortcode_from_filename, slugify};
use crate::sync::{SharedMatrix, SharedStatus, Syncer};

pub struct AppState {
    pub cfg: Config,
    pub db: Db,
    pub syncer: Syncer,
    pub matrix: SharedMatrix,
    pub status: SharedStatus,
    /// ffmpeg is CPU heavy; at most two conversions at once.
    pub convert: Semaphore,
    /// Serialises count checks + inserts (the 100-per-category limit).
    pub write: Mutex<()>,
    pub access: crate::access::Cache,
}

pub type AppStateRef = Arc<AppState>;

pub struct ApiError(StatusCode, String);

impl ApiError {
    pub fn new(code: StatusCode, msg: impl Into<String>) -> Self {
        Self(code, msg.into())
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(e: E) -> Self {
        let e = e.into();
        tracing::error!("{e:#}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

fn not_found(what: &str) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, format!("{what} not found"))
}

fn conflict(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, msg)
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, msg)
}

pub fn router(state: AppStateRef) -> Router<AppStateRef> {
    use crate::admin;
    let admin_only = Router::new()
        .route("/api/admin/rooms", post(admin::add_room))
        .route("/api/admin/rooms/{id}", patch(admin::update_room).delete(admin::delete_room))
        .route("/api/admin/rooms/{id}/join", post(admin::join_room))
        .route("/api/admin/users", get(admin::list_users).post(admin::add_user))
        .route("/api/admin/users/{user}", patch(admin::update_user).delete(admin::delete_user))
        .route_layer(axum::middleware::from_fn(auth::require_admin));
    let protected = Router::new()
        .route("/api/status", get(status))
        .route("/api/resync", post(resync))
        .route("/api/rooms", get(admin::list_rooms))
        .route("/api/categories", get(list_categories).post(create_category))
        .route(
            "/api/categories/{slug}",
            get(get_category).patch(update_category).delete(delete_category),
        )
        .route("/api/categories/{slug}/rooms", put(set_category_rooms))
        .route("/api/categories/{slug}/stickers", get(list_stickers).post(upload_sticker))
        .route("/api/stickers/{id}", patch(update_sticker).delete(delete_sticker))
        .route("/files/{name}", get(file))
        .merge(admin_only)
        .route_layer(axum::middleware::from_fn_with_state(state, auth::require));
    Router::new()
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/config", get(config))
        .route("/api/login", post(auth::login))
        .route("/api/logout", post(auth::logout))
        .route("/api/auth/matrix/start", get(auth::matrix_start))
        .route("/api/auth/matrix/callback", get(auth::matrix_callback))
        .merge(protected)
}

async fn config(State(st): State<AppStateRef>, headers: HeaderMap) -> Json<Value> {
    let access = auth::access(&st, &headers).await;
    let bot = st.status.read().await.user_id.clone();
    Json(json!({
        "auth": {
            "enabled": auth::auth_enabled(&st),
            "pin": st.cfg.pin.is_some(),
            "matrix": auth::matrix_login_enabled(&st),
        },
        "logged_in": access.is_some(),
        "user": access.as_ref().and_then(|a| a.user.clone()),
        "role": access.as_ref().map(|a| a.role),
        "bot": bot,
        "max_per_category": st.cfg.max_per_category,
        "max_upload_mb": st.cfg.max_upload_bytes / 1024 / 1024,
        "max_px": st.cfg.max_px,
        "target_kb": st.cfg.target_bytes / 1000,
    }))
}

async fn status(State(st): State<AppStateRef>) -> ApiResult<Json<Value>> {
    let s = st.status.read().await.clone();
    let pending_uploads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stickers WHERE mxc_uri IS NULL")
        .fetch_one(&st.db)
        .await?;
    let failing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM categories WHERE sync_error IS NOT NULL")
        .fetch_one(&st.db)
        .await?;
    Ok(Json(json!({ "matrix": s, "pending_uploads": pending_uploads, "failing_packs": failing })))
}

async fn resync(State(st): State<AppStateRef>) -> StatusCode {
    st.syncer.full();
    StatusCode::ACCEPTED
}

// ---- categories ------------------------------------------------------------

async fn category_json(st: &AppState, c: &Category) -> ApiResult<Value> {
    let count = db::count_in(&st.db, c.id).await?;
    let preview: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM stickers WHERE category_id = ? ORDER BY created_at, id LIMIT 4",
    )
    .bind(c.id)
    .fetch_all(&st.db)
    .await?;
    let rooms: Vec<String> = sqlx::query_scalar("SELECT room_id FROM category_rooms WHERE category_id = ?")
        .bind(c.id)
        .fetch_all(&st.db)
        .await?;
    let mut v = serde_json::to_value(c)?;
    v["rooms"] = json!(rooms);
    v["count"] = json!(count);
    v["max"] = json!(st.cfg.max_per_category);
    v["preview"] = json!(preview.iter().map(|id| format!("/files/{id}.webp")).collect::<Vec<_>>());
    Ok(v)
}

async fn load_category(st: &AppState, slug: &str) -> ApiResult<Category> {
    db::category_by_slug(&st.db, slug).await?.ok_or_else(|| not_found("Category"))
}

async fn list_categories(State(st): State<AppStateRef>) -> ApiResult<Json<Vec<Value>>> {
    let mut out = Vec::new();
    for c in db::categories(&st.db).await? {
        out.push(category_json(&st, &c).await?);
    }
    Ok(Json(out))
}

async fn get_category(State(st): State<AppStateRef>, Path(slug): Path<String>) -> ApiResult<Json<Value>> {
    let c = load_category(&st, &slug).await?;
    Ok(Json(category_json(&st, &c).await?))
}

#[derive(Deserialize)]
struct NewCategory {
    display_name: String,
    slug: Option<String>,
}

async fn create_category(
    State(st): State<AppStateRef>,
    Json(req): Json<NewCategory>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let name = req.display_name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(bad_request("Name must be 1–64 characters"));
    }
    let slug = slugify(req.slug.as_deref().filter(|s| !s.trim().is_empty()).unwrap_or(name), 48);
    if slug.is_empty() {
        return Err(bad_request("Could not derive a slug from the name, give one explicitly"));
    }
    let _w = st.write.lock().await;
    if db::category_by_slug(&st.db, &slug).await?.is_some() {
        return Err(conflict(format!("Category \"{slug}\" already exists")));
    }
    sqlx::query("INSERT INTO categories (slug, display_name, created_at) VALUES (?, ?, ?)")
        .bind(&slug)
        .bind(name)
        .bind(db::now())
        .execute(&st.db)
        .await?;
    let c = load_category(&st, &slug).await?;
    sqlx::query(
        "INSERT INTO category_rooms (category_id, room_id) SELECT ?, room_id FROM rooms WHERE default_publish = 1",
    )
    .bind(c.id)
    .execute(&st.db)
    .await?;
    Ok((StatusCode::CREATED, Json(category_json(&st, &c).await?)))
}

#[derive(Deserialize)]
struct UpdateCategory {
    display_name: String,
}

async fn update_category(
    State(st): State<AppStateRef>,
    Path(slug): Path<String>,
    Json(req): Json<UpdateCategory>,
) -> ApiResult<Json<Value>> {
    let c = load_category(&st, &slug).await?;
    let name = req.display_name.trim();
    if name.is_empty() || name.chars().count() > 64 {
        return Err(bad_request("Name must be 1–64 characters"));
    }
    sqlx::query("UPDATE categories SET display_name = ? WHERE id = ?")
        .bind(name)
        .bind(c.id)
        .execute(&st.db)
        .await?;
    st.syncer.queue(&slug);
    let c = load_category(&st, &slug).await?;
    Ok(Json(category_json(&st, &c).await?))
}

#[derive(Deserialize)]
struct DeleteQuery {
    #[serde(default)]
    force: bool,
}

async fn delete_category(
    State(st): State<AppStateRef>,
    Path(slug): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    let _w = st.write.lock().await;
    let c = load_category(&st, &slug).await?;
    let stickers = db::stickers_in(&st.db, c.id).await?;
    if !stickers.is_empty() && !q.force {
        return Err(conflict(format!("Category still has {} stickers", stickers.len())));
    }
    for s in &stickers {
        if let Some(mxc) = &s.mxc_uri {
            db::trash_media(&st.db, &slug, mxc).await?;
        }
    }
    sqlx::query("DELETE FROM categories WHERE id = ?").bind(c.id).execute(&st.db).await?;
    for s in &stickers {
        let _ = tokio::fs::remove_file(crate::files_path(s.id)).await;
    }
    st.syncer.queue(&slug);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CategoryRooms {
    rooms: Vec<String>,
}

async fn set_category_rooms(
    State(st): State<AppStateRef>,
    Path(slug): Path<String>,
    Json(req): Json<CategoryRooms>,
) -> ApiResult<Json<Value>> {
    let c = load_category(&st, &slug).await?;
    let known: Vec<String> = sqlx::query_scalar("SELECT room_id FROM rooms").fetch_all(&st.db).await?;
    if let Some(bad) = req.rooms.iter().find(|r| !known.contains(r)) {
        return Err(bad_request(format!("{bad} is not a configured room")));
    }
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM category_rooms WHERE category_id = ?").bind(c.id).execute(&mut *tx).await?;
    for r in &req.rooms {
        sqlx::query("INSERT OR IGNORE INTO category_rooms (category_id, room_id) VALUES (?, ?)")
            .bind(c.id)
            .bind(r)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    st.syncer.queue(&slug);
    Ok(Json(category_json(&st, &c).await?))
}

// ---- stickers --------------------------------------------------------------

fn sticker_json(s: &Sticker, slug: &str) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or_default();
    v["category"] = json!(slug);
    v["url"] = json!(format!("/files/{}.webp", s.id));
    v
}

async fn list_stickers(State(st): State<AppStateRef>, Path(slug): Path<String>) -> ApiResult<Json<Vec<Value>>> {
    let c = load_category(&st, &slug).await?;
    Ok(Json(db::stickers_in(&st.db, c.id).await?.iter().map(|s| sticker_json(s, &slug)).collect()))
}

/// Matrix user id when logged in via Matrix, else the self-declared name
/// header (PIN / open mode).
fn created_by(access: &Access, headers: &HeaderMap) -> Option<String> {
    if let Some(u) = access.matrix_user() {
        return Some(u.to_string());
    }
    let raw = headers.get("x-gifmeme-user")?.to_str().ok()?;
    let name = urlencoding::decode(raw).ok()?.trim().chars().take(64).collect::<String>();
    (!name.is_empty()).then_some(name)
}

fn clean_shortcode(raw: &str) -> ApiResult<String> {
    let s = slugify(raw.trim().trim_matches(':'), 64);
    if s.is_empty() {
        return Err(bad_request("Shortcodes may only contain a–z, 0–9, - and _"));
    }
    Ok(s)
}

async fn upload_sticker(
    State(st): State<AppStateRef>,
    Path(slug): Path<String>,
    Extension(access): Extension<Access>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let c = load_category(&st, &slug).await?;
    let max = st.cfg.max_per_category;
    if db::count_in(&st.db, c.id).await? >= max {
        return Err(conflict(format!("Category is full ({max}/{max})")));
    }

    let mut shortcode: Option<String> = None;
    let mut body: Option<String> = None;
    let mut upload: Option<(String, tempfile::NamedTempFile, String)> = None;
    while let Some(mut field) = mp.next_field().await.map_err(|e| bad_request(e.to_string()))? {
        match field.name() {
            Some("shortcode") => shortcode = Some(field.text().await.map_err(|e| bad_request(e.to_string()))?),
            Some("body") => body = Some(field.text().await.map_err(|e| bad_request(e.to_string()))?),
            Some("file") => {
                let filename = field.file_name().unwrap_or("sticker").to_string();
                let tmp = tempfile::Builder::new().prefix("upload-").tempfile_in(st.cfg.tmp_dir())?;
                let mut f = tokio::fs::File::create(tmp.path()).await?;
                let mut hasher = Sha256::new();
                let mut total = 0usize;
                while let Some(chunk) = field.chunk().await.map_err(|e| bad_request(e.to_string()))? {
                    total += chunk.len();
                    if total > st.cfg.max_upload_bytes {
                        return Err(ApiError::new(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            format!("File is larger than {} MB", st.cfg.max_upload_bytes / 1024 / 1024),
                        ));
                    }
                    hasher.update(&chunk);
                    f.write_all(&chunk).await?;
                }
                f.flush().await?;
                if total == 0 {
                    return Err(bad_request("Empty file"));
                }
                upload = Some((filename, tmp, hex::encode(hasher.finalize())));
            }
            _ => {}
        }
    }
    let (filename, tmp, sha) = upload.ok_or_else(|| bad_request("No file in the form (field \"file\")"))?;

    if let Some(existing) = sqlx::query_scalar::<_, String>(
        "SELECT shortcode FROM stickers WHERE category_id = ? AND sha256 = ?",
    )
    .bind(c.id)
    .bind(&sha)
    .fetch_optional(&st.db)
    .await?
    {
        return Err(conflict(format!("Already in this category as :{existing}:")));
    }

    let converted = {
        let _permit = st.convert.acquire().await?;
        crate::convert::to_webp(tmp.path(), &st.cfg.tmp_dir(), st.cfg.max_px, st.cfg.target_bytes)
            .await
            .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))?
    };
    drop(tmp);

    let base = match shortcode.as_deref().filter(|s| !s.trim().is_empty()) {
        Some(s) => clean_shortcode(s)?,
        None => shortcode_from_filename(&filename),
    };
    let stored_name = format!("{base}.webp");
    let size = converted.bytes.len() as i64;

    let id = {
        let _w = st.write.lock().await;
        // Re-check under the lock: parallel uploads race for the last slots.
        if db::count_in(&st.db, c.id).await? >= max {
            return Err(conflict(format!("Category is full ({max}/{max})")));
        }
        let shortcode = db::unique_shortcode(&st.db, c.id, &base, None).await?;
        let res = sqlx::query(
            "INSERT INTO stickers (category_id, shortcode, body, filename, w, h, size, mimetype, sha256, created_by, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, 'image/webp', ?, ?, ?)",
        )
        .bind(c.id)
        .bind(&shortcode)
        .bind(body.map(|b| b.trim().chars().take(200).collect::<String>()).filter(|b| !b.is_empty()))
        .bind(&stored_name)
        .bind(converted.w as i64)
        .bind(converted.h as i64)
        .bind(size)
        .bind(&sha)
        .bind(created_by(&access, &headers))
        .bind(db::now())
        .execute(&st.db)
        .await;
        let id = match res {
            Ok(r) => r.last_insert_rowid(),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(conflict("The same file was just uploaded to this category"));
            }
            Err(e) => return Err(e.into()),
        };
        let path = crate::files_path(id);
        let part = path.with_extension("part");
        let write = async {
            tokio::fs::write(&part, &converted.bytes).await?;
            tokio::fs::rename(&part, &path).await
        };
        if let Err(e) = write.await {
            let _ = sqlx::query("DELETE FROM stickers WHERE id = ?").bind(id).execute(&st.db).await;
            return Err(e.into());
        }
        id
    };

    // Upload right away so the pack sync only has to PUT; if Synapse is
    // unreachable the syncer uploads it later (mxc_uri stays NULL).
    let m = st.matrix.read().await.clone();
    if let Some(m) = m {
        match m.upload(&stored_name, "image/webp", converted.bytes.into()).await {
            Ok(mxc) => {
                sqlx::query("UPDATE stickers SET mxc_uri = ? WHERE id = ?")
                    .bind(&mxc)
                    .bind(id)
                    .execute(&st.db)
                    .await?;
            }
            Err(e) => tracing::warn!(id, "upload failed, syncer will retry: {e:#}"),
        }
    }
    st.syncer.queue(&slug);

    let s = db::sticker(&st.db, id).await?.ok_or_else(|| not_found("Sticker"))?;
    Ok((StatusCode::CREATED, Json(sticker_json(&s, &slug))))
}

#[derive(Deserialize)]
struct UpdateSticker {
    shortcode: Option<String>,
    body: Option<String>,
    category: Option<String>,
}

async fn update_sticker(
    State(st): State<AppStateRef>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateSticker>,
) -> ApiResult<Json<Value>> {
    let _w = st.write.lock().await;
    let s = db::sticker(&st.db, id).await?.ok_or_else(|| not_found("Sticker"))?;
    let from = db::category_by_id(&st.db, s.category_id).await?.ok_or_else(|| not_found("Category"))?;
    let to = match req.category.as_deref() {
        Some(slug) if slug != from.slug => load_category(&st, slug).await?,
        _ => from.clone(),
    };
    let moving = to.id != from.id;

    if moving {
        let max = st.cfg.max_per_category;
        if db::count_in(&st.db, to.id).await? >= max {
            return Err(conflict(format!("{} is full ({max}/{max})", to.display_name)));
        }
        let dup: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM stickers WHERE category_id = ? AND sha256 = ?")
            .bind(to.id)
            .bind(&s.sha256)
            .fetch_one(&st.db)
            .await?;
        if dup > 0 {
            return Err(conflict(format!("The same file is already in {}", to.display_name)));
        }
    }

    let shortcode = match req.shortcode.as_deref() {
        Some(raw) => {
            let sc = clean_shortcode(raw)?;
            if db::shortcode_taken(&st.db, to.id, &sc, Some(id)).await? {
                return Err(conflict(format!(":{sc}: already exists in {}", to.display_name)));
            }
            sc
        }
        // Moving keeps the shortcode unless it collides in the target pack.
        None if moving => db::unique_shortcode(&st.db, to.id, &s.shortcode, Some(id)).await?,
        None => s.shortcode.clone(),
    };
    let body = match req.body {
        Some(b) => Some(b.trim().chars().take(200).collect::<String>()).filter(|b| !b.is_empty()),
        None => s.body.clone(),
    };

    sqlx::query("UPDATE stickers SET shortcode = ?, body = ?, category_id = ? WHERE id = ?")
        .bind(&shortcode)
        .bind(&body)
        .bind(to.id)
        .bind(id)
        .execute(&st.db)
        .await?;
    st.syncer.queue(&from.slug);
    if moving {
        st.syncer.queue(&to.slug);
    }
    let s = db::sticker(&st.db, id).await?.ok_or_else(|| not_found("Sticker"))?;
    Ok(Json(sticker_json(&s, &to.slug)))
}

async fn delete_sticker(State(st): State<AppStateRef>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let _w = st.write.lock().await;
    let s = db::sticker(&st.db, id).await?.ok_or_else(|| not_found("Sticker"))?;
    let c = db::category_by_id(&st.db, s.category_id).await?.ok_or_else(|| not_found("Category"))?;
    if let Some(mxc) = &s.mxc_uri {
        // Deleted from Synapse only after the pack stopped referencing it.
        db::trash_media(&st.db, &c.slug, mxc).await?;
    }
    sqlx::query("DELETE FROM stickers WHERE id = ?").bind(id).execute(&st.db).await?;
    let _ = tokio::fs::remove_file(crate::files_path(id)).await;
    st.syncer.queue(&c.slug);
    Ok(StatusCode::NO_CONTENT)
}

async fn file(Path(name): Path<String>) -> ApiResult<Response> {
    let id: i64 = name
        .strip_suffix(".webp")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| not_found("File"))?;
    let bytes = match tokio::fs::read(crate::files_path(id)).await {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found("File")),
        Err(e) => return Err(e.into()),
    };
    Ok((
        [
            (header::CONTENT_TYPE, "image/webp"),
            // ids are never reused for different content
            (header::CACHE_CONTROL, "private, max-age=31536000, immutable"),
        ],
        bytes,
    )
        .into_response())
}
