mod app_state;
mod import;
mod routes;
mod transmission;

use std::net::SocketAddr;

use axum::{routing::{delete, get, post}, Router};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;
use tracing::info;

use crate::app_state::AppState;
use crate::import::{reconcile_auto_import_task, stop_auto_import_task};
use crate::routes::{
    add_to_transmission, api_setup, delete_history_item, health, home, history, import_item,
    search, setup_page, transmission_torrents,
};

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let state = AppState::load()?;
    reconcile_auto_import_task(state.clone()).await;

    let app = Router::new()
        .route("/health", get(health))
        .route("/", get(home))
        .route("/setup", get(setup_page))
        .route("/api/setup", post(api_setup))
        .route("/search", post(search))
        .route("/add", post(add_to_transmission))
        .route("/history", get(history))
        .route("/history/:row_id", delete(delete_history_item))
        .route("/transmission/torrents", get(transmission_torrents))
        .route("/import", post(import_item))
        .nest_service("/static", ServeDir::new("app/static"))
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stop_auto_import_task(state.clone()).await;
        })
        .await?;
    Ok(())
}
