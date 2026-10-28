//! Who may use the dashboard, and as what.
//!
//! - `users` table: explicit admins/editors.
//! - Bootstrap: while there is no admin, the first Matrix user to log in
//!   becomes one.
//! - Room rules: members of a configured room with power level >=
//!   `login_min_power` are editors.
//!
//! Room-rule answers are cached for a few minutes, so losing power in a room
//! revokes access without waiting for the session cookie to expire.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use tokio::sync::Mutex;

use crate::api::AppState;
use crate::db;

const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Admin,
    Editor,
}

impl Role {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "admin" => Some(Self::Admin),
            "editor" => Some(Self::Editor),
            _ => None,
        }
    }
}

/// Inserted into request extensions by the auth middleware.
#[derive(Clone, Debug)]
pub struct Access {
    /// Matrix user id, "pin", or None when auth is off.
    pub user: Option<String>,
    pub role: Role,
}

impl Access {
    pub fn matrix_user(&self) -> Option<&str> {
        self.user.as_deref().filter(|u| u.starts_with('@'))
    }
}

#[derive(Default)]
pub struct Cache(Mutex<HashMap<String, (Option<Role>, Instant)>>);

impl Cache {
    pub async fn clear(&self) {
        self.0.lock().await.clear();
    }

    pub async fn forget(&self, user: &str) {
        self.0.lock().await.remove(user);
    }
}

/// Cached role lookup for a Matrix user.
pub async fn role_cached(st: &AppState, user: &str) -> Result<Option<Role>> {
    if let Some((role, at)) = st.access.0.lock().await.get(user) {
        if at.elapsed() < CACHE_TTL {
            return Ok(*role);
        }
    }
    let role = resolve(st, user).await?;
    st.access.0.lock().await.insert(user.to_string(), (role, Instant::now()));
    Ok(role)
}

pub async fn resolve(st: &AppState, user: &str) -> Result<Option<Role>> {
    if let Some(role) = sqlx::query_scalar::<_, String>("SELECT role FROM users WHERE user_id = ?")
        .bind(user)
        .fetch_optional(&st.db)
        .await?
    {
        return Ok(Role::parse(&role));
    }

    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = 'admin'")
        .fetch_one(&st.db)
        .await?;
    if admins == 0 {
        let res = sqlx::query(
            "INSERT INTO users (user_id, role, added_by, created_at) VALUES (?, 'admin', 'first login', ?)
             ON CONFLICT DO NOTHING",
        )
        .bind(user)
        .bind(db::now())
        .execute(&st.db)
        .await?;
        if res.rows_affected() == 1 {
            tracing::warn!(%user, "no admin yet: {user} claimed admin");
            return Ok(Some(Role::Admin));
        }
    }

    let Some(m) = st.matrix.read().await.clone() else {
        return Ok(None);
    };
    let rules: Vec<(String, i64)> =
        sqlx::query_as("SELECT room_id, login_min_power FROM rooms WHERE login_min_power IS NOT NULL")
            .fetch_all(&st.db)
            .await?;
    for (room, min) in rules {
        match m.member_power(&room, user).await {
            Ok(Some(p)) if p >= min => return Ok(Some(Role::Editor)),
            Ok(_) => {}
            Err(e) => tracing::warn!(%room, %user, "power check failed: {e:#}"),
        }
    }
    Ok(None)
}
