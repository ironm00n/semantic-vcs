use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

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
    #[error("repository slug {slug:?} appears in both {first} and {second}")]
    DuplicateSlug {
        slug: String,
        first: String,
        second: String,
    },
    #[error("at least one forge catalog is required")]
    NoCatalogs,
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

    /// Merge independently exported catalogs. Slugs are routing keys, so ambiguity is rejected.
    pub fn load_many(paths: &[PathBuf]) -> Result<Self, Error> {
        if paths.is_empty() {
            return Err(Error::NoCatalogs);
        }
        let mut repositories = Vec::new();
        let mut origins = BTreeMap::<String, String>::new();
        for path in paths {
            let origin = path.display().to_string();
            for repository in Self::load(path)?.repositories {
                if let Some(first) = origins.insert(repository.slug.clone(), origin.clone()) {
                    return Err(Error::DuplicateSlug {
                        slug: repository.slug,
                        first,
                        second: origin,
                    });
                }
                repositories.push(repository);
            }
        }
        Ok(Self { repositories })
    }

    fn repository(&self, slug: &str) -> Option<&Repository> {
        self.repositories.iter().find(|repo| repo.slug == slug)
    }
}

#[derive(Clone)]
enum CatalogSource {
    Static(Catalog),
    Files(Vec<PathBuf>),
}

impl CatalogSource {
    fn load(&self) -> Result<Catalog, Error> {
        match self {
            Self::Static(catalog) => Ok(catalog.clone()),
            // Reopening on every request observes an exporter’s atomic rename without a watcher,
            // stale cache window, or redb access from the web process.
            Self::Files(paths) => Catalog::load_many(paths),
        }
    }
}

type AppState = Arc<CatalogSource>;

pub fn app(catalog: Catalog) -> Router {
    router(CatalogSource::Static(catalog))
}

/// Validate all catalogs before binding, then reload them for every API request.
pub fn app_from_paths(paths: Vec<PathBuf>) -> Result<Router, Error> {
    Catalog::load_many(&paths)?;
    Ok(router(CatalogSource::Files(paths)))
}

fn router(source: CatalogSource) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/repositories", get(repositories))
        .route("/api/repositories/{slug}", get(repository))
        .route("/api/repositories/{slug}/snapshots", get(snapshots))
        .route("/api/repositories/{slug}/snapshots/{id}", get(snapshot))
        .route("/api/repositories/{slug}/operations", get(operations))
        .route("/api/repositories/{slug}/reviews", get(reviews))
        .with_state(Arc::new(source))
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

fn current(source: &AppState) -> Result<Catalog, (StatusCode, String)> {
    source
        .load()
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))
}

async fn repositories(State(source): State<AppState>) -> impl IntoResponse {
    // The response owns its Arc until serialization; extending these borrows is not sound, so
    // return owned summaries through the helper below instead.
    match current(&source) {
        Ok(catalog) => Json(
            catalog
                .repositories
                .iter()
                .map(OwnedRepositorySummary::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(error) => error.into_response(),
    }
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
            slug: summary.slug.into(),
            name: summary.name.into(),
            description: summary.description.into(),
            head: summary.head.into(),
            snapshots: summary.snapshots,
            operations: summary.operations,
            conflicts: summary.conflicts,
            reviews: summary.reviews,
        }
    }
}

async fn repository(
    State(source): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> impl IntoResponse {
    let catalog = match current(&source) {
        Ok(catalog) => catalog,
        Err(error) => return error.into_response(),
    };
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn snapshots(
    State(source): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> impl IntoResponse {
    let catalog = match current(&source) {
        Ok(catalog) => catalog,
        Err(error) => return error.into_response(),
    };
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.snapshots.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn snapshot(
    State(source): State<AppState>,
    AxumPath((slug, id)): AxumPath<(String, String)>,
) -> impl IntoResponse {
    let catalog = match current(&source) {
        Ok(catalog) => catalog,
        Err(error) => return error.into_response(),
    };
    match catalog
        .repository(&slug)
        .and_then(|repo| repo.snapshots.iter().find(|s| s.id == id))
    {
        Some(snapshot) => Json(snapshot.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "snapshot not found").into_response(),
    }
}

async fn operations(
    State(source): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> impl IntoResponse {
    let catalog = match current(&source) {
        Ok(catalog) => catalog,
        Err(error) => return error.into_response(),
    };
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.operations.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}

async fn reviews(
    State(source): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> impl IntoResponse {
    let catalog = match current(&source) {
        Ok(catalog) => catalog,
        Err(error) => return error.into_response(),
    };
    match catalog.repository(&slug) {
        Some(repo) => Json(repo.review_queue.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "repository not found").into_response(),
    }
}
