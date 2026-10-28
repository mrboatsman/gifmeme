mod access;
mod admin;
mod api;
mod auth;
mod config;
mod convert;
mod db;
mod dns;
mod matrix;
mod slug;
mod sync;

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use axum::extract::DefaultBodyLimit;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

static FILES_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Converted sticker on disk; the DB row id names the file.
pub fn files_path(id: i64) -> PathBuf {
    FILES_DIR.get().expect("files dir initialised").join(format!("{id}.webp"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "gifmeme=info,tower_http=warn".into()))
        .init();

    let cfg = config::Config::from_env()?;
    for dir in [cfg.data_dir.clone(), cfg.files_dir(), cfg.tmp_dir()] {
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    // Leftovers from uploads interrupted by a restart.
    if let Ok(entries) = std::fs::read_dir(cfg.tmp_dir()) {
        for e in entries.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    FILES_DIR.set(cfg.files_dir()).ok();

    let db = db::open(&cfg.data_dir.join("gifmeme.db")).await?;
    let matrix = Arc::new(RwLock::new(None));
    let status = Arc::new(RwLock::new(sync::Status::default()));
    let syncer = sync::start(cfg.clone(), db.clone(), matrix.clone(), status.clone());

    let state = Arc::new(api::AppState {
        cfg: cfg.clone(),
        db,
        syncer,
        matrix,
        status,
        convert: Semaphore::new(2),
        write: Mutex::new(()),
        access: Default::default(),
    });

    let index = cfg.static_dir.join("200.html");
    let spa = ServeDir::new(&cfg.static_dir).fallback(ServeFile::new(index));
    let app = api::router(state.clone())
        .fallback_service(spa)
        .layer(DefaultBodyLimit::max(cfg.max_upload_bytes + 1024 * 1024))
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cfg.listen).await?;
    tracing::info!("listening on http://{}", cfg.listen);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("sigterm");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {},
                _ = term.recv() => {},
            }
        })
        .await?;
    Ok(())
}
