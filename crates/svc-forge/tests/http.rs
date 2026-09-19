use axum::{body::Body, http::{Request, StatusCode}};
use svc_forge::{Catalog, Repository, SnapshotView};
use tower::ServiceExt;

fn catalog() -> Catalog {
    Catalog { repositories: vec![Repository {
        slug: "svc".into(), name: "svc".into(), path: "/tmp/svc".into(),
        description: "semantic vcs".into(), head: "s1".into(), operations: vec![],
        review_queue: vec![], snapshots: vec![SnapshotView {
            id: "s1".into(), change: "c1".into(), message: "first".into(),
            parents: vec![], entities: vec![], conflicts: vec![],
        }],
    }]}
}

#[tokio::test]
async fn serves_ui_and_repository_views() {
    let app = svc_forge::app(catalog());
    for (path, status) in [
        ("/", StatusCode::OK),
        ("/api/repositories", StatusCode::OK),
        ("/api/repositories/svc", StatusCode::OK),
        ("/api/repositories/svc/snapshots/s1", StatusCode::OK),
        ("/api/repositories/svc/operations", StatusCode::OK),
        ("/api/repositories/svc/reviews", StatusCode::OK),
        ("/api/repositories/missing", StatusCode::NOT_FOUND),
    ] {
        let response = app.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
}

#[test]
fn example_catalog_tracks_the_dogfood_repository() {
    let parsed: Catalog = serde_json::from_str(include_str!("../examples/forge.json")).unwrap();
    assert_eq!(parsed.repositories[0].slug, "svc");
    assert_eq!(parsed.repositories[0].snapshots[0].id, "demo-head");
}
