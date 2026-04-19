mod handlers;

use crate::index::Index;
use axum::{routing::get, Router};
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub index: Index,
    pub jdk_src: Option<crate::jdk::JdkSrc>,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(handlers::root))
        .route("/health", get(handlers::health))
        .route("/md/:fqn", get(handlers::md))
        .route("/html/:fqn", get(handlers::html_redirect))
        .route("/jar/:artifact/*path", get(handlers::jar_asset))
        .route("/search", get(handlers::search))
        .route("/tree", get(handlers::tree))
        .route("/tree/classes", get(handlers::tree_classes))
        .route("/tree/html", get(handlers::tree_html))
        .route("/source", get(handlers::source_docs))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}
