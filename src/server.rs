use crate::bytecode;
use crate::extract;
use crate::index::Index;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

pub struct AppState {
    pub index: Index,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/md/:fqn", get(md))
        .route("/html/:fqn", get(html_redirect))
        .route("/jar/:artifact/*path", get(jar_asset))
        .route("/search", get(search))
        .route("/source", get(source_docs))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn root() -> &'static str {
    "ijdocs-server: GET /md/<fqn>, /html/<fqn>, /jar/<artifact>/<path>, /search?q="
}

async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
struct MdQuery {
    member: Option<String>,
    jar: Option<String>,
}

#[derive(serde::Serialize)]
struct MdResponse {
    markdown: String,
    artifact: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    maven_coord: Option<String>,
    fqn: String,
    type_refs: std::collections::HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    annotations: Option<crate::bytecode::MethodAnnotations>,
}

async fn md(
    State(state): State<Arc<AppState>>,
    Path(fqn): Path<String>,
    Query(q): Query<MdQuery>,
) -> Response {
    let (jar_idx, entry) = match state.index.resolve_with_hint(&fqn, q.jar.as_deref()) {
        Some(v) => v,
        None => return (StatusCode::NOT_FOUND, format!("unknown fqn: {fqn}")).into_response(),
    };
    let jar = state.index.jar(jar_idx).expect("jar");
    let html = match extract::read_jar_entry(&jar.path, &entry) {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let annotations = jar.classes_jar.as_ref().and_then(|classes_path| {
        let class_entry = entry.replace(".html", ".class");
        bytecode::extract_annotations(classes_path, &class_entry)
    });

    let result = extract::html_to_markdown(&html, q.member.as_deref(), annotations.as_ref(), Some(&fqn));
    Json(MdResponse {
        markdown: result.markdown,
        artifact: jar.artifact_id.clone(),
        maven_coord: jar.maven_coord.clone(),
        fqn,
        type_refs: result.type_refs,
        annotations,
    })
    .into_response()
}

async fn html_redirect(
    State(state): State<Arc<AppState>>,
    Path(fqn): Path<String>,
) -> Response {
    let (jar_idx, entry) = match state.index.resolve(&fqn) {
        Some(v) => v,
        None => return (StatusCode::NOT_FOUND, format!("unknown fqn: {fqn}")).into_response(),
    };
    let jar = state.index.jar(jar_idx).expect("jar");
    let url = format!("/jar/{}/{}", jar.artifact_id, entry);
    Redirect::temporary(&url).into_response()
}

async fn jar_asset(
    State(state): State<Arc<AppState>>,
    Path((artifact, path)): Path<(String, String)>,
) -> Response {
    let (_idx, jar) = match state.index.jar_by_artifact(&artifact) {
        Some(v) => v,
        None => return (StatusCode::NOT_FOUND, "unknown artifact").into_response(),
    };
    let bytes = match extract::read_jar_entry(&jar.path, &path) {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, format!("missing entry {path}")).into_response(),
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.essence_str().to_string())], bytes).into_response()
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    50
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Json<Vec<crate::index::SearchHit>> {
    Json(state.index.search(&q.q, q.limit))
}

#[derive(Deserialize)]
struct SourceQuery {
    path: String,
    member: Option<String>,
}

#[derive(serde::Serialize)]
struct SourceResponse {
    markdown: String,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    type_refs: std::collections::HashMap<String, String>,
}

async fn source_docs(Query(q): Query<SourceQuery>) -> Response {
    let path = std::path::Path::new(&q.path);
    if !path.is_file() || !q.path.ends_with(".java") {
        return (StatusCode::NOT_FOUND, format!("not a java file: {}", q.path)).into_response();
    }
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let result = extract::source_to_markdown(&source, q.member.as_deref());
    Json(SourceResponse {
        markdown: result.markdown,
        type_refs: result.type_refs,
    })
    .into_response()
}
