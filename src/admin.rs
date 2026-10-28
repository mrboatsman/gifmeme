//! Rooms and users, configured in the UI.

use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::access::{Access, Role};
use crate::api::{ApiError, ApiResult, AppState, AppStateRef};
use crate::db;

fn err(code: StatusCode, msg: impl Into<String>) -> ApiError {
    ApiError::new(code, msg)
}

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct RoomRow {
    pub id: i64,
    pub room_id: String,
    pub alias: Option<String>,
    pub name: Option<String>,
    pub login_min_power: Option<i64>,
    pub default_publish: bool,
    pub added_by: Option<String>,
    pub created_at: i64,
}

pub async fn rooms(db: &db::Db) -> ApiResult<Vec<RoomRow>> {
    Ok(sqlx::query_as::<_, RoomRow>("SELECT * FROM rooms ORDER BY COALESCE(name, alias, room_id) COLLATE NOCASE")
        .fetch_all(db)
        .await?)
}

/// Rooms with the bot's live view (joined? may it write packs?), refreshing
/// the cached name/alias on the way.
pub async fn list_rooms(State(st): State<AppStateRef>) -> ApiResult<Json<Vec<Value>>> {
    let m = st.matrix.read().await.clone();
    let mut out = Vec::new();
    for r in rooms(&st.db).await? {
        let mut v = serde_json::to_value(&r)?;
        let packs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM category_rooms WHERE room_id = ?")
            .bind(&r.room_id)
            .fetch_one(&st.db)
            .await?;
        v["categories"] = json!(packs);
        if let Some(m) = &m {
            let info = m.room_info(&r.room_id).await;
            if info.name.is_some() || info.alias.is_some() {
                sqlx::query("UPDATE rooms SET name = COALESCE(?, name), alias = COALESCE(?, alias) WHERE id = ?")
                    .bind(&info.name)
                    .bind(&info.alias)
                    .bind(r.id)
                    .execute(&st.db)
                    .await?;
                if info.name.is_some() {
                    v["name"] = json!(info.name);
                }
                if info.alias.is_some() {
                    v["alias"] = json!(info.alias);
                }
            }
            v["bot"] = json!({
                "joined": info.bot_joined,
                "power": info.bot_power.min(1_000_000),
                "needed": info.needed_power,
                "can_write": info.bot_can_write(),
                "error": info.error,
            });
        }
        out.push(v);
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct NewRoom {
    room: String,
    login_min_power: Option<i64>,
    #[serde(default = "yes")]
    default_publish: bool,
}

fn yes() -> bool {
    true
}

pub async fn add_room(
    State(st): State<AppStateRef>,
    Extension(who): Extension<Access>,
    Json(req): Json<NewRoom>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let m = st
        .matrix
        .read()
        .await
        .clone()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "The backend cannot reach Matrix right now"))?;
    let input = req.room.trim();
    let room_id = m
        .resolve_room(input)
        .await
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    // Joining works for public rooms and pending invites; otherwise the room
    // is added anyway and the UI tells the admin to invite the bot.
    if let Err(e) = m.join(&room_id).await {
        tracing::info!(%room_id, "bot could not join yet: {e:#}");
    }
    let info = m.room_info(&room_id).await;
    let res = sqlx::query(
        "INSERT INTO rooms (room_id, alias, name, login_min_power, default_publish, added_by, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&room_id)
    .bind(info.alias.clone().or_else(|| input.starts_with('#').then(|| input.to_string())))
    .bind(&info.name)
    .bind(req.login_min_power)
    .bind(req.default_publish)
    .bind(who.user.clone())
    .bind(db::now())
    .execute(&st.db)
    .await;
    match res {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(err(StatusCode::CONFLICT, "That room is already configured"));
        }
        Err(e) => return Err(e.into()),
    }
    st.access.clear().await;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "room_id": room_id, "bot": { "joined": info.bot_joined, "can_write": info.bot_can_write() } })),
    ))
}

#[derive(Deserialize)]
pub struct UpdateRoom {
    /// null = the room grants no logins
    login_min_power: Option<i64>,
    default_publish: bool,
}

pub async fn update_room(
    State(st): State<AppStateRef>,
    Path(id): Path<i64>,
    Json(req): Json<UpdateRoom>,
) -> ApiResult<StatusCode> {
    let res = sqlx::query("UPDATE rooms SET login_min_power = ?, default_publish = ? WHERE id = ?")
        .bind(req.login_min_power)
        .bind(req.default_publish)
        .bind(id)
        .execute(&st.db)
        .await?;
    if res.rows_affected() == 0 {
        return Err(err(StatusCode::NOT_FOUND, "Room not found"));
    }
    st.access.clear().await;
    Ok(StatusCode::NO_CONTENT)
}

/// Retry joining (after the admin invited the bot).
pub async fn join_room(State(st): State<AppStateRef>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let room_id: String = sqlx::query_scalar("SELECT room_id FROM rooms WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Room not found"))?;
    let m = st
        .matrix
        .read()
        .await
        .clone()
        .ok_or_else(|| err(StatusCode::SERVICE_UNAVAILABLE, "The backend cannot reach Matrix right now"))?;
    m.join(&room_id).await.map_err(|e| {
        err(StatusCode::BAD_REQUEST, format!("Could not join, invite {} first ({e:#})", m.user_id))
    })?;
    // Packs that should be there but failed while the bot was out.
    let slugs: Vec<String> = sqlx::query_scalar(
        "SELECT c.slug FROM categories c JOIN category_rooms cr ON cr.category_id = c.id WHERE cr.room_id = ?",
    )
    .bind(&room_id)
    .fetch_all(&st.db)
    .await?;
    for s in slugs {
        st.syncer.queue(&s);
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Stops publishing there; the syncer blanks the packs it put in that room.
pub async fn delete_room(State(st): State<AppStateRef>, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    let room_id: String = sqlx::query_scalar("SELECT room_id FROM rooms WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "Room not found"))?;
    let slugs: Vec<String> = sqlx::query_scalar("SELECT slug FROM publications WHERE room_id = ?")
        .bind(&room_id)
        .fetch_all(&st.db)
        .await?;
    sqlx::query("DELETE FROM rooms WHERE id = ?").bind(id).execute(&st.db).await?;
    for s in slugs {
        st.syncer.queue(&s);
    }
    st.access.clear().await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- users -------------------------------------------------------------------

#[derive(sqlx::FromRow, serde::Serialize)]
pub struct UserRow {
    user_id: String,
    role: String,
    added_by: Option<String>,
    created_at: i64,
    last_login: Option<i64>,
}

pub async fn list_users(State(st): State<AppStateRef>) -> ApiResult<Json<Vec<UserRow>>> {
    Ok(Json(
        sqlx::query_as::<_, UserRow>("SELECT * FROM users ORDER BY role, user_id")
            .fetch_all(&st.db)
            .await?,
    ))
}

#[derive(Deserialize)]
pub struct UserReq {
    user_id: Option<String>,
    role: String,
}

fn valid_mxid(s: &str) -> bool {
    s.starts_with('@') && s.contains(':') && !s.contains(char::is_whitespace) && s.len() <= 255
}

async fn admin_count(st: &AppState) -> ApiResult<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'").fetch_one(&st.db).await?)
}

pub async fn add_user(
    State(st): State<AppStateRef>,
    Extension(who): Extension<Access>,
    Json(req): Json<UserReq>,
) -> ApiResult<StatusCode> {
    let user = req.user_id.as_deref().unwrap_or("").trim().to_string();
    if !valid_mxid(&user) {
        return Err(err(StatusCode::BAD_REQUEST, "Expected a Matrix user id like @name:server"));
    }
    let role = Role::parse(&req.role).ok_or_else(|| err(StatusCode::BAD_REQUEST, "Role must be admin or editor"))?;
    let res = sqlx::query("INSERT INTO users (user_id, role, added_by, created_at) VALUES (?, ?, ?, ?)")
        .bind(&user)
        .bind(&req.role)
        .bind(who.user.clone())
        .bind(db::now())
        .execute(&st.db)
        .await;
    match res {
        Ok(_) => {}
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(err(StatusCode::CONFLICT, format!("{user} is already listed")));
        }
        Err(e) => return Err(e.into()),
    }
    tracing::info!(%user, ?role, by = ?who.user, "user added");
    st.access.clear().await;
    Ok(StatusCode::CREATED)
}

pub async fn update_user(
    State(st): State<AppStateRef>,
    Path(user): Path<String>,
    Json(req): Json<UserReq>,
) -> ApiResult<StatusCode> {
    Role::parse(&req.role).ok_or_else(|| err(StatusCode::BAD_REQUEST, "Role must be admin or editor"))?;
    let current: String = sqlx::query_scalar("SELECT role FROM users WHERE user_id = ?")
        .bind(&user)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "User not found"))?;
    if current == "admin" && req.role != "admin" && admin_count(&st).await? <= 1 {
        return Err(err(StatusCode::CONFLICT, "Cannot demote the last admin"));
    }
    sqlx::query("UPDATE users SET role = ? WHERE user_id = ?")
        .bind(&req.role)
        .bind(&user)
        .execute(&st.db)
        .await?;
    st.access.clear().await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_user(State(st): State<AppStateRef>, Path(user): Path<String>) -> ApiResult<StatusCode> {
    let current: String = sqlx::query_scalar("SELECT role FROM users WHERE user_id = ?")
        .bind(&user)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(|| err(StatusCode::NOT_FOUND, "User not found"))?;
    if current == "admin" && admin_count(&st).await? <= 1 {
        return Err(err(StatusCode::CONFLICT, "Cannot remove the last admin"));
    }
    sqlx::query("DELETE FROM users WHERE user_id = ?").bind(&user).execute(&st.db).await?;
    st.access.clear().await;
    Ok(StatusCode::NO_CONTENT)
}
