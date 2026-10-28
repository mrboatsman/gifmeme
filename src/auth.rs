//! Dashboard login. Two optional methods:
//!
//! - Matrix (GIFMEME_PUBLIC_URL set): SSO through the homeserver. The browser
//!   goes to /_matrix/client/v3/login/sso/redirect, comes back to
//!   /api/auth/matrix/callback with a loginToken, the backend trades it for
//!   the user id (and revokes the minted access token), checks access
//!   (crate::access) and sets a signed session cookie.
//! - Shared PIN (GIFMEME_PIN set).
//!
//! With neither configured the dashboard is open (NetBird is the perimeter).
//! Sessions are HMAC(bot token)-signed `user|expiry`, so rotating the bot
//! token logs everyone out.

use std::time::Duration;

use axum::Json;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use hmac::{Hmac, KeyInit, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use urlencoding::encode;

use crate::access::{Access, Role};
use crate::api::{AppState, AppStateRef};
use crate::db::now;

const SESSION: &str = "gifmeme_session";
const STATE: &str = "gifmeme_sso_state";
/// Session user for PIN logins (no identity).
pub const PIN_USER: &str = "pin";

#[derive(Clone, Debug)]
pub struct Session {
    /// Matrix user id, or PIN_USER, or None when auth is off.
    pub user: Option<String>,
}

pub fn auth_enabled(st: &AppState) -> bool {
    st.cfg.pin.is_some() || matrix_login_enabled(st)
}

pub fn matrix_login_enabled(st: &AppState) -> bool {
    st.cfg.public_url.is_some()
}

fn mac(st: &AppState, data: &str) -> String {
    let key = st.cfg.access_token.as_deref().unwrap_or("gifmeme-dev");
    let mut m = Hmac::<Sha256>::new_from_slice(key.as_bytes()).expect("hmac accepts any key");
    m.update(b"gifmeme-session-v2:");
    m.update(data.as_bytes());
    // The PIN is part of PIN sessions so changing it revokes them.
    if data.starts_with(&format!("{}|", hex::encode(PIN_USER))) {
        m.update(st.cfg.pin.as_deref().unwrap_or("").as_bytes());
    }
    hex::encode(m.finalize().into_bytes())
}

fn session_cookie(st: &AppState, user: &str) -> String {
    let max_age = st.cfg.session_days * 86400;
    let data = format!("{}|{}", hex::encode(user), now() + max_age);
    format!(
        "{SESSION}={data}|{}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}",
        mac(st, &data)
    )
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn session(st: &AppState, headers: &HeaderMap) -> Option<Session> {
    if !auth_enabled(st) {
        return Some(Session { user: None });
    }
    let raw = cookie(headers, SESSION)?;
    let (data, sig) = raw.rsplit_once('|')?;
    if !ct_eq(sig.as_bytes(), mac(st, data).as_bytes()) {
        return None;
    }
    let (user_hex, exp) = data.split_once('|')?;
    if exp.parse::<i64>().ok()? < now() {
        return None;
    }
    let user = String::from_utf8(hex::decode(user_hex).ok()?).ok()?;
    // A PIN session stops working once the PIN is removed, and vice versa.
    let valid = if user == PIN_USER { st.cfg.pin.is_some() } else { matrix_login_enabled(st) };
    valid.then_some(Session { user: Some(user) })
}

/// Session -> Access (role), or None when the user has no (more) access.
pub async fn access(st: &AppState, headers: &HeaderMap) -> Option<Access> {
    let s = session(st, headers)?;
    match s.user.as_deref() {
        // Auth off: everyone is an admin.
        None => Some(Access { user: None, role: Role::Admin }),
        // PIN alone is the only gate when Matrix login is off.
        Some(PIN_USER) => Some(Access {
            user: s.user.clone(),
            role: if matrix_login_enabled(st) { Role::Editor } else { Role::Admin },
        }),
        Some(user) => match crate::access::role_cached(st, user).await {
            Ok(Some(role)) => Some(Access { user: s.user.clone(), role }),
            Ok(None) => None,
            Err(e) => {
                tracing::error!(%user, "access check failed: {e:#}");
                None
            }
        },
    }
}

pub async fn require(State(st): State<AppStateRef>, mut req: Request, next: Next) -> Response {
    match access(&st, req.headers()).await {
        Some(a) => {
            req.extensions_mut().insert(a);
            next.run(req).await
        }
        None => (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Log in first" }))).into_response(),
    }
}

/// Must run inside `require` (needs the Access extension).
pub async fn require_admin(req: Request, next: Next) -> Response {
    match req.extensions().get::<Access>() {
        Some(a) if a.role == Role::Admin => next.run(req).await,
        _ => (StatusCode::FORBIDDEN, Json(json!({ "error": "Admins only" }))).into_response(),
    }
}

// ---- PIN ---------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Login {
    pin: String,
}

pub async fn login(State(st): State<AppStateRef>, Json(req): Json<Login>) -> Response {
    let Some(pin) = &st.cfg.pin else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "PIN login is disabled" }))).into_response();
    };
    if !ct_eq(req.pin.trim().as_bytes(), pin.as_bytes()) {
        // Slow down guessing; good enough behind the VPN.
        tokio::time::sleep(Duration::from_secs(1)).await;
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "Wrong PIN" }))).into_response();
    }
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, session_cookie(&st, PIN_USER))]).into_response()
}

pub async fn logout() -> Response {
    let cookie = format!("{SESSION}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

// ---- Matrix SSO -------------------------------------------------------------

fn back_to_login(error: &str) -> Response {
    let clear = format!("{STATE}=; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=0");
    ([(header::SET_COOKIE, clear)], Redirect::to(&format!("/?login_error={}", encode(error)))).into_response()
}

pub async fn matrix_start(State(st): State<AppStateRef>) -> Response {
    let Some(public) = &st.cfg.public_url else {
        return back_to_login("Matrix login is not configured");
    };
    let m = st.matrix.read().await.clone();
    let Some(m) = m else {
        return back_to_login("The backend cannot reach Matrix right now");
    };
    // Straight to the provider when there is exactly one (Pocket ID here);
    // otherwise Synapse shows its picker.
    let idp = match m.sso_providers().await {
        Ok(ps) if ps.len() == 1 => Some(ps[0].clone()),
        Ok(_) => None,
        Err(e) => return back_to_login(&format!("{e:#}")),
    };
    let mut nonce = [0u8; 16];
    if getrandom::fill(&mut nonce).is_err() {
        return back_to_login("no randomness available");
    }
    let state = hex::encode(nonce);
    let callback = format!("{public}/api/auth/matrix/callback?state={state}");
    let hs = st
        .cfg
        .matrix_public_url
        .as_deref()
        .or(st.cfg.homeserver.as_deref())
        .unwrap_or_default();
    let target = match idp {
        Some(idp) => format!(
            "{hs}/_matrix/client/v3/login/sso/redirect/{}?redirectUrl={}",
            encode(&idp),
            encode(&callback)
        ),
        None => format!("{hs}/_matrix/client/v3/login/sso/redirect?redirectUrl={}", encode(&callback)),
    };
    // Lax: sent on the top-level redirect back from the homeserver.
    let cookie = format!("{STATE}={state}; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=600");
    ([(header::SET_COOKIE, cookie)], Redirect::to(&target)).into_response()
}

#[derive(Deserialize)]
pub struct Callback {
    state: Option<String>,
    #[serde(rename = "loginToken")]
    login_token: Option<String>,
}

pub async fn matrix_callback(
    State(st): State<AppStateRef>,
    headers: HeaderMap,
    Query(q): Query<Callback>,
) -> Response {
    let (Some(state), Some(token)) = (q.state, q.login_token) else {
        return back_to_login("Login was cancelled");
    };
    match cookie(&headers, STATE) {
        Some(expected) if ct_eq(expected.as_bytes(), state.as_bytes()) => {}
        _ => return back_to_login("Login expired, please try again"),
    }
    let m = st.matrix.read().await.clone();
    let Some(m) = m else {
        return back_to_login("The backend cannot reach Matrix right now");
    };
    let user = match m.whoami_from_login_token(&token).await {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("SSO token exchange failed: {e:#}");
            return back_to_login(&format!("Matrix rejected the login ({e:#})"));
        }
    };
    // Fresh answer, not the cache: this is where access is granted.
    match crate::access::resolve(&st, &user).await {
        Ok(Some(role)) => {
            st.access.forget(&user).await;
            let _ = sqlx::query("UPDATE users SET last_login = ? WHERE user_id = ?")
                .bind(now())
                .bind(&user)
                .execute(&st.db)
                .await;
            tracing::info!(%user, ?role, "access granted");
        }
        Ok(None) => {
            tracing::info!(%user, "login refused: no access");
            return back_to_login(&format!(
                "{user} has no access. Ask an admin to add you, or join a room that grants access."
            ));
        }
        Err(e) => return back_to_login(&format!("could not check access: {e:#}")),
    }
    tracing::info!(%user, "logged in");
    let clear = format!("{STATE}=; Path=/api/auth; HttpOnly; SameSite=Lax; Max-Age=0");
    let mut res = Redirect::to("/").into_response();
    let h = res.headers_mut();
    h.append(header::SET_COOKIE, session_cookie(&st, &user).parse().unwrap());
    h.append(header::SET_COOKIE, clear.parse().unwrap());
    res
}
