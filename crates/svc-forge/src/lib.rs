use std::{path::Path, sync::Arc};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use svc_core::{Conflict, EntityRecord, OpLogEntry, ReviewItem};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not read forge catalog {path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("invalid forge catalog {path}: {source}")]
    Parse { path: String, source: serde_json::Error },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub repositories: Vec<Repository>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repository {
    /// URL-safe stable repository key.
    pub slug: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub description: String,
    pub head: String,
    #[serde(default)]
    pub snapshots: Vec<SnapshotView>,
    #[serde(default)]
    pub operations: Vec<OpLogEntry>,
    #[serde(default)]
    pub review_queue: Vec<ReviewItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapshotView {
    pub id: String,
    pub change: String,
    pub message: String,
    #[serde(default)]
    pub parents: Vec<String>,
    #[serde(default)]
    pub entities: Vec<EntityView>,
    #[serde(default)]
    pub conflicts: Vec<Conflict>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityView {
    pub id: String,
    #[serde(flatten)]
    pub record: EntityRecord,
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let display = path.display().to_string();
        let bytes = std::fs::read(path).map_err(|source| Error::Read {
            path: display.clone(),
            source,
        })?;
        serde_json::from_slice(&bytes).map_err(|source| Error::Parse {
            path: display,
            source,
        })
    }

    fn repository(&self, slug: &str) -> Option<&Repository> {
        self.repositories.iter().find(|repo| repo.slug == slug)
    }
}

type AppState = Arc<Catalog>;

pub fn app(catalog: Catalog) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/repositories", get(repositories))
        .route("/api/repositories/{slug}", get(repository))
        .route("/api/repositories/{slug}/snapshots", get(snapshots))
        .route("/api/repositories/{slug}/snapshots/{id}", get(snapshot))
        .route("/api/repositories/{slug}/operations", get(operations))
        .route("/api/repositories/{slug}/reviews", get(reviews))
        .with_state(Arc::new(catalog))
}

async fn index() -> Html<&'static str> {
    Html(include_str!("index.html"))
}

#[derive(Serialize)]
struct RepositorySummary<'a> {
    slug: &'a str,
    name: &'a str,
    description: &'a str,
    head: &'a str,
    snapshots: usize,
    operations: usize,
    conflicts: usize,
    reviews: usize,
}

impl<'a> From<&'a Repository> for RepositorySummary<'a> {
    fn from(repo: &'a Repository) -> Self {
        Self {
            slug: &repo.slug,
            name: &repo.name,
            description: &repo.description,
            head: &repo.head,
            snapshots: repo.snapshots.len(),
            operations: repo.operations.len(),
            conflicts: repo.snapshots.iter().map(|s| s.conflicts.len()).sum(),
            reviews: repo.review_queue.len(),
        }
    }
}

async fn repositories(State(catalog): State<AppState>) -> Json<Vec<OwnedRepositorySummary>> {
    // The response owns its Arc until serialization; extending these borrows is not sound, so
    // return owned summaries through the helper below instead.
    let owned = catalog.repositories.iter().map(OwnedRepositorySummary::from).collect();
    Json(owned)
}

#[derive(Serialize)]
struct OwnedRepositorySummary {
    slug: String,
    name: String,
    description: String,
    head: String,
    snapshots: usize,
    operations: usize,
    conflicts: usize,
    reviews: usize,
}

impl From<&Repository> for OwnedRepositorySummary {
    fn from(repo: &Repository) -> Self {
        let summary = RepositorySummary::from(repo);
        Self {
            slug: summary.slug.into(), name: summary.name.into(),
            description: summary.description.into(), head: summary.head.into(),
            snapshots: summary.snapshots, operations: summary.operations,
            conflicts: summary.conflicts, reviews: summary.reviews,
        }
    }
}

async fn repository(State(catalog): State<AppState>, AxumPath(slug): AxumPath<String>) -> impl IntoResponse {
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn snapshots(State(catalog): State<AppState>, AxumPath(slug): AxumPath<String>) -> impl IntoResponse {
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.snapshots.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn snapshot(State(catalog): State<AppState>, AxumPath((slug, id)): AxumPath<(String, String)>) -> impl IntoResponse {
    match catalog.repository(&slug).and_then(|repo| repo.snapshots.iter().find(|s| s.id == id)) {
        Some(snapshot) => Json(snapshot.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "snapshot not found").into_response(),
    }
}

async fn operations(State(catalog): State<AppState>, AxumPath(slug): AxumPath<String>) -> impl IntoResponse {
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.operations.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn reviews(State(catalog): State<AppState>, AxumPath(slug): AxumPath<String>) -> impl IntoResponse {
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.review_queue.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}
