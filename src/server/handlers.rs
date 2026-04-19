use super::AppState;
use crate::bytecode;
use crate::extract;
use axum::{
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;
use std::sync::Arc;

pub async fn root() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
        include_str!("html/tree.html"),
    )
        .into_response()
}

pub async fn health() -> &'static str {
    "ok"
}

#[derive(Deserialize)]
pub(crate) struct MdQuery {
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    has_html: bool,
}

pub async fn md(
    State(state): State<Arc<AppState>>,
    Path(fqn): Path<String>,
    Query(q): Query<MdQuery>,
) -> Response {
    let (jar_idx, entry) = match state.index.resolve_with_hint(&fqn, q.jar.as_deref()) {
        Some(v) => v,
        None => {
            if let Some(jdk) = &state.jdk_src {
                if let Some(source) = jdk.read_source(&fqn) {
                    let result =
                        extract::source_to_markdown(&source, q.member.as_deref());
                    return Json(MdResponse {
                        markdown: result.markdown,
                        artifact: "jdk".to_string(),
                        maven_coord: None,
                        fqn,
                        type_refs: result.type_refs,
                        annotations: None,
                        has_html: false,
                    })
                    .into_response();
                }
            }
            return (StatusCode::NOT_FOUND, format!("unknown fqn: {fqn}")).into_response();
        }
    };
    let jar = state.index.jar(jar_idx).expect("jar");

    if entry.ends_with(".class") {
        let md = format!("```\n{fqn}\n```\n\n_(no javadoc or source available — bytecode only)_\n");
        return Json(MdResponse {
            markdown: md,
            artifact: jar.artifact_id.clone(),
            maven_coord: jar.maven_coord.clone(),
            fqn,
            type_refs: std::collections::HashMap::new(),
            annotations: None,
            has_html: false,
        })
        .into_response();
    }

    let raw = match extract::read_jar_entry(&jar.path, &entry) {
        Ok(b) => b,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    if jar.is_sources {
        let source = String::from_utf8_lossy(&raw);
        let result = extract::source_to_markdown(&source, q.member.as_deref());
        return Json(MdResponse {
            markdown: result.markdown,
            artifact: jar.artifact_id.clone(),
            maven_coord: jar.maven_coord.clone(),
            fqn,
            type_refs: result.type_refs,
            annotations: None,
            has_html: false,
        })
        .into_response();
    }

    let annotations = jar.classes_jar.as_ref().and_then(|classes_path| {
        let class_entry = entry.replace(".html", ".class");
        bytecode::extract_annotations(classes_path, &class_entry)
    });

    let result = extract::html_to_markdown(&raw, q.member.as_deref(), annotations.as_ref(), Some(&fqn));
    Json(MdResponse {
        markdown: result.markdown,
        artifact: jar.artifact_id.clone(),
        maven_coord: jar.maven_coord.clone(),
        fqn,
        type_refs: result.type_refs,
        annotations,
        has_html: true,
    })
    .into_response()
}

pub async fn html_redirect(
    State(state): State<Arc<AppState>>,
    Path(fqn): Path<String>,
) -> Response {
    let (jar_idx, entry) = match state.index.resolve(&fqn) {
        Some(v) => v,
        None => {
            if let Some(jdk) = &state.jdk_src {
                if let Some(url) = jdk.docs_url(&fqn) {
                    return Redirect::temporary(&url).into_response();
                }
            }
            return (StatusCode::NOT_FOUND, format!("unknown fqn: {fqn}")).into_response();
        }
    };
    if entry.ends_with(".class") {
        return (StatusCode::NOT_FOUND, "no HTML docs available (bytecode only)").into_response();
    }
    let jar = state.index.jar(jar_idx).expect("jar");
    if jar.is_sources {
        return (StatusCode::NOT_FOUND, "no HTML docs available (source only)").into_response();
    }
    let url = format!("/jar/{}/{}", jar.artifact_id, entry);
    Redirect::temporary(&url).into_response()
}

pub async fn jar_asset(
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
pub(crate) struct SearchQuery {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    50
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Json<Vec<crate::index::SearchHit>> {
    let mut hits = state.index.search(&q.q, q.limit);
    for hit in &mut hits {
        if hit.jar_idx == crate::index::JDK_JAR_IDX {
            hit.artifact = "jdk".into();
        }
    }
    Json(hits)
}

pub async fn tree(
    State(state): State<Arc<AppState>>,
) -> Json<Vec<crate::index::ArtifactInfo>> {
    let mut artifacts = state.index.list_artifacts();
    if let Some(jdk) = &state.jdk_src {
        let jdk_packages = jdk.package_list();
        if !jdk_packages.is_empty() {
            artifacts.insert(0, crate::index::ArtifactInfo {
                artifact: "jdk".to_string(),
                maven_coord: None,
                jar_idx: crate::index::JDK_JAR_IDX,
                packages: jdk_packages,
            });
        }
    }
    Json(artifacts)
}

#[derive(Deserialize)]
pub(crate) struct TreeClassesQuery {
    package: String,
    jar_idx: usize,
}

pub async fn tree_classes(
    State(state): State<Arc<AppState>>,
    Query(q): Query<TreeClassesQuery>,
) -> Json<Vec<String>> {
    if q.jar_idx == crate::index::JDK_JAR_IDX {
        if let Some(jdk) = &state.jdk_src {
            return Json(jdk.list_classes(&q.package));
        }
        return Json(Vec::new());
    }
    Json(state.index.list_classes(q.jar_idx, &q.package))
}

pub async fn tree_html() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
        include_str!("html/tree.html"),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(crate) struct SourceQuery {
    path: String,
    member: Option<String>,
}

#[derive(serde::Serialize)]
struct SourceResponse {
    markdown: String,
    #[serde(skip_serializing_if = "std::collections::HashMap::is_empty")]
    type_refs: std::collections::HashMap<String, String>,
}

pub async fn source_docs(Query(q): Query<SourceQuery>) -> Response {
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
