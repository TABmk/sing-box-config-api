mod config;
mod routes;

use anyhow::{Context, Result};
use axum::Router;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::config::{
    RuntimeConfig, describe_config_source, ensure_secure_secret, load_runtime_config,
};

#[derive(Clone)]
pub(crate) struct AppState {
    runtime_config: Arc<RuntimeConfig>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let runtime_config = load_runtime_config().context("failed to load runtime config")?;
    ensure_secure_secret(&runtime_config)?;

    let listener = TcpListener::bind(&runtime_config.settings.listen_addr)
        .await
        .with_context(|| {
            format!(
                "failed to bind API listener to {}",
                runtime_config.settings.listen_addr
            )
        })?;

    let state = AppState {
        runtime_config: Arc::new(runtime_config),
    };

    let app = Router::new()
        .nest("/api", routes::api_router(state.clone()))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state.clone());

    println!(
        "listening on {} with config source {}",
        state.runtime_config.settings.listen_addr,
        describe_config_source(state.runtime_config.config_source.as_deref())
    );

    axum::serve(listener, app)
        .await
        .context("axum server exited unexpectedly")
}
