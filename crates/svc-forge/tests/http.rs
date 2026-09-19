use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use svc_core::{
    BytesId, Conflict, ContentId, EntityId, EntityRecord, Intent, Kind, Op, OpIx,
    OpLogEntry, RelPath, ReviewItem, Side, SnapshotId, View,
};
use svc_forge::{Catalog, EntityView, Repository, SnapshotView};
use tower::ServiceExt;

struct TestDir(std::path::PathBuf);

impl TestDir {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "svc-forge-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn op(operation: Op) -> OpLogEntry {
    let view = View {
        root: SnapshotId([1; 32]),
        heads: Default::default(),
    };
    OpLogEntry {
        op: operation,
        observed: None,
        at: 1,
        group: None,
        before: view.clone(),
        after: view,
    }
}

fn catalog() -> Catalog {
    let conflict = Conflict::DeleteEdit {
        id: EntityId::new(),
        deleted_by: Side::A,
        edited_by: Side::B,
    };
    Catalog {
        repositories: vec![Repository {
            slug: "svc".into(),
            name: "svc".into(),
            path: "/tmp/svc".into(),
            description: "semantic vcs".into(),
            head: "s2".into(),
            operations: vec![
                op(Op::Rename {
                    id: EntityId::new(),
                    new: "parse_config".into(),
                }),
                op(Op::Undo),
            ],
            review_queue: vec![
                ReviewItem::EditReview {
                    op: OpIx(0),
                    declared: Intent::Fix,
                    observed: None,
                    ask_id: None,
                },
                ReviewItem::BindingConflict {
                    conflict: conflict.clone(),
                },
            ],
            snapshots: vec![
                SnapshotView {
                    id: "s1".into(),
                    change: "c1".into(),
                    message: "first".into(),
                    parents: vec![],
                    entities: vec![],
                    conflicts: vec![],
                },
                SnapshotView {
                    id: "s2".into(),
                    change: "c2".into(),
                    message: "rename parser".into(),
                    parents: vec!["s1".into()],
                    entities: vec![EntityView {
                        id: EntityId::new().to_string(),
                        record: EntityRecord {
                            name: "parse_config".into(),
                            kind: Kind::Fn,
                            parent: None,
                            file: RelPath::new("src/main.rs").unwrap(),
                            ordinal: 0,
                            content: ContentId([2; 32]),
                            bytes: BytesId([3; 32]),
                        },
                    }],
                    conflicts: vec![conflict],
                },
            ],
        }],
    }
}

async fn response(app: axum::Router, path: &str) -> (StatusCode, String) {
    let response = app
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let text = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, text)
}

fn write_catalog(path: &std::path::Path, slug: &str, head: &str) {
    let mut value = catalog();
    value.repositories[0].slug = slug.into();
    value.repositories[0].name = slug.into();
    value.repositories[0].head = head.into();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[tokio::test]
async fn serves_ui_and_repository_views() {
    let app = svc_forge::app(catalog());
    for (path, status) in [
        ("/", StatusCode::OK),
        ("/api/repositories", StatusCode::OK),
        ("/api/repositories/svc", StatusCode::OK),
        ("/api/repositories/svc/snapshots", StatusCode::OK),
        ("/api/repositories/svc/snapshots/s1", StatusCode::OK),
        ("/api/repositories/svc/operations", StatusCode::OK),
        ("/api/repositories/svc/reviews", StatusCode::OK),
        ("/api/repositories/missing", StatusCode::NOT_FOUND),
    ] {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{path}");
    }
}

#[tokio::test]
async fn populated_repository_response_preserves_semantic_types() {
    let (status, text) = response(svc_forge::app(catalog()), "/api/repositories/svc").await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["head"], "s2");
    assert_eq!(value["snapshots"][1]["change"], "c2");
    assert_eq!(
        value["snapshots"][1]["entities"][0]["name"],
        "parse_config"
    );
    assert!(value["snapshots"][1]["conflicts"][0]["DeleteEdit"].is_object());
    assert_eq!(
        value["operations"][0]["op"]["Rename"]["new"],
        "parse_config"
    );
    assert_eq!(value["operations"][1]["op"], "Undo");
    assert!(value["review_queue"][0]["EditReview"].is_object());
    assert!(value["review_queue"][1]["BindingConflict"].is_object());
}

#[tokio::test]
async fn browser_contract_has_typed_labels_change_navigation_and_entity_filters() {
    let (status, html) = response(svc_forge::app(catalog()), "/").await;
    assert_eq!(status, StatusCode::OK);
    for contract in [
        "typeof op==='string'?op",
        "Changes & head",
        "data-snapshot",
        "selectSnapshot",
        "entity-query",
        "entity-kind",
        "PAGE_SIZE=100",
        "filteredEntities",
        "conflictSummary",
        "Delete/edit conflict",
        "Binding safety review",
        "selectEntity",
        "Parent reference",
        "Content identity",
        "Raw entity record",
        "@media(max-width:760px)",
        "overflow-wrap:anywhere",
        "word-break:break-word",
    ] {
        assert!(html.contains(contract), "missing browser contract: {contract}");
    }
}

#[test]
fn example_catalog_tracks_the_dogfood_repository() {
    let parsed: Catalog =
        serde_json::from_str(include_str!("../examples/forge.json")).unwrap();
    assert_eq!(parsed.repositories[0].slug, "svc");
    assert_eq!(parsed.repositories[0].snapshots[0].id, "demo-head");
}

#[tokio::test]
async fn file_source_merges_two_catalogs() {
    let dir = TestDir::new();
    let alpha = dir.path().join("alpha.json");
    let beta = dir.path().join("beta.json");
    write_catalog(&alpha, "alpha", "alpha-head");
    write_catalog(&beta, "beta", "beta-head");

    let app = svc_forge::app_from_paths(vec![alpha, beta]).unwrap();
    let (_, text) = response(app.clone(), "/api/repositories").await;
    let repositories: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(repositories.as_array().unwrap().len(), 2);
    assert_eq!(repositories[0]["slug"], "alpha");
    assert_eq!(repositories[1]["slug"], "beta");

    let (status, text) = response(app, "/api/repositories/beta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&text).unwrap()["head"], "beta-head");
}

#[test]
fn file_source_rejects_duplicate_slugs() {
    let dir = TestDir::new();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    write_catalog(&first, "same", "first-head");
    write_catalog(&second, "same", "second-head");

    match svc_forge::app_from_paths(vec![first.clone(), second.clone()]) {
        Err(svc_forge::Error::DuplicateSlug {
            slug,
            first: a,
            second: b,
        }) => {
            assert_eq!(slug, "same");
            assert_eq!(a, first.display().to_string());
            assert_eq!(b, second.display().to_string());
        }
        _ => panic!("duplicate slug was accepted"),
    }
}

#[tokio::test]
async fn file_source_observes_atomic_catalog_replacement() {
    let dir = TestDir::new();
    let live = dir.path().join("forge.json");
    let replacement = dir.path().join("forge.next.json");
    write_catalog(&live, "svc", "old-head");
    let app = svc_forge::app_from_paths(vec![live.clone()]).unwrap();

    let (_, before) = response(app.clone(), "/api/repositories/svc").await;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&before).unwrap()["head"], "old-head");

    write_catalog(&replacement, "svc", "new-head");
    std::fs::rename(&replacement, &live).unwrap();
    let (_, after) = response(app, "/api/repositories/svc").await;
    assert_eq!(serde_json::from_str::<serde_json::Value>(&after).unwrap()["head"], "new-head");
}
