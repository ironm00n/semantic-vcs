use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use svc_core::{
    BytesId, Conflict, ContentId, EntityId, EntityRecord, Intent, Kind, Op, OpIx, OpLogEntry,
    RelPath, ReviewItem, Side, SnapshotId, View,
};
use svc_forge::{
    Catalog, EntityView, OperationSubject, OperationView, Repository, SnapshotView, TouchView,
};
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

fn op(operation: Op) -> OperationView {
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
    .into()
}

fn catalog() -> Catalog {
    let entity = EntityId::new();
    let conflict = Conflict::DeleteEdit {
        id: EntityId::new(),
        deleted_by: Side::A,
        edited_by: Side::B,
    };
    let mut rename = op(Op::Rename {
        id: entity,
        new: "parse_config".into(),
    });
    rename.ix = Some(7);
    rename.change = Some("c2".into());
    rename.group_name = Some("sol: parser refactor".into());
    rename.subject = Some(OperationSubject {
        id: entity.to_string(),
        before_name: "parse".into(),
        after_name: "parse_config".into(),
        kind: "Fn".into(),
        file: "src/main.rs".into(),
        before_source: "fn parse() {}".into(),
        after_source: "fn parse_config() {}".into(),
        before_source_html: String::new(),
        after_source_html: String::new(),
        touch: serde_json::json!({"Renamed":{"from":"parse","to":"parse_config"}}),
    });
    rename.workspace = Some("sol".into());
    rename.subjects = vec![
        TouchView {
            entity: entity.to_string(),
            name: "parse_config".into(),
            touch: serde_json::json!({"Renamed":{"from":"parse","to":"parse_config"}}),
            kind: "Fn".into(),
            file: "src/main.rs".into(),
            before_source: "fn parse() {}".into(),
            after_source: "fn parse_config() {}".into(),
            before_source_html: String::new(),
            after_source_html: String::new(),
        },
        TouchView {
            entity: EntityId::new().to_string(),
            name: "caller".into(),
            touch: serde_json::json!({"Edited":{"observed":"BindingPreserving"}}),
            kind: "Fn".into(),
            file: "src/main.rs".into(),
            before_source: "fn caller() { parse() }".into(),
            after_source: "fn caller() { parse_config() }".into(),
            before_source_html: String::new(),
            after_source_html: String::new(),
        },
    ];
    Catalog {
        repositories: vec![Repository {
            slug: "svc".into(),
            name: "svc".into(),
            path: "/tmp/svc".into(),
            description: "semantic vcs".into(),
            head: "s2".into(),
            heads: vec!["s2".into()],
            operations: vec![rename, op(Op::Undo)],
            review_queue: vec![
                ReviewItem::EditReview {
                    op: OpIx(7),
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
                        id: entity.to_string(),
                        source: "fn parse_config() {}".into(),
                        source_html: String::new(),
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

fn write_forge_catalog(path: &std::path::Path, slug: &str, head: &str) {
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
    let app = svc_forge::app(catalog());
    let (status, text) = response(app.clone(), "/api/repositories/svc").await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["head"], "s2");
    assert_eq!(value["heads"], serde_json::json!(["s2"]));
    assert_eq!(value["snapshots"][1]["change"], "c2");
    assert_eq!(value["snapshots"][1]["entities"][0]["name"], "parse_config");
    assert_eq!(
        value["snapshots"][1]["entities"][0]["source"],
        "fn parse_config() {}"
    );
    assert!(value["snapshots"][1]["conflicts"][0]["DeleteEdit"].is_object());
    assert_eq!(
        value["operations"][0]["op"]["Rename"]["new"],
        "parse_config"
    );
    assert_eq!(value["operations"][0]["ix"], 7);
    assert_eq!(value["operations"][0]["change"], "c2");
    assert_eq!(value["operations"][0]["group_name"], "sol: parser refactor");
    assert_eq!(value["operations"][0]["subject"]["before_name"], "parse");
    assert!(
        value["operations"][0]["subject"]["before_source_html"]
            .as_str()
            .unwrap()
            .contains("syntax-keyword")
    );
    assert_eq!(value["operations"][1]["op"], "Undo");
    assert!(value["review_queue"][0]["EditReview"].is_object());
    assert!(value["review_queue"][1]["BindingConflict"].is_object());

    let entity = value["snapshots"][1]["entities"][0]["id"].as_str().unwrap();
    let path = format!("/api/repositories/svc/entities/{entity}/source");
    let (status, source) = response(app, &path).await;
    assert_eq!(status, StatusCode::OK);
    let source: serde_json::Value = serde_json::from_str(&source).unwrap();
    assert!(
        source["source_html"]
            .as_str()
            .unwrap()
            .contains("syntax-keyword")
    );
}

#[tokio::test]
async fn operation_subjects_are_served_highlighted_per_op() {
    let app = svc_forge::app(catalog());
    let (status, body) = response(app.clone(), "/api/repositories/svc").await;
    assert_eq!(status, StatusCode::OK);
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    // The repository carries the touches and the checkout, but not the highlighting.
    assert_eq!(value["operations"][0]["workspace"], "sol");
    assert_eq!(value["operations"][0]["subjects"].as_array().unwrap().len(), 2);
    assert!(value["operations"][0]["subjects"][1]["after_source_html"].is_null());
    assert!(value["operations"][1]["subjects"].is_null());

    let (status, body) = response(app.clone(), "/api/repositories/svc/operations/7/subjects").await;
    assert_eq!(status, StatusCode::OK);
    let touches: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(touches.as_array().unwrap().len(), 2);
    assert_eq!(touches[1]["name"], "caller");
    assert!(touches[1]["before_source_html"].as_str().unwrap().contains("syntax-keyword"));
    assert!(touches[1]["after_source_html"].as_str().unwrap().contains("parse_config"));

    let (status, _) = response(app, "/api/repositories/svc/operations/99/subjects").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn add_and_delete_subjects_accept_null_source_sides() {
    let id = EntityId::new();
    let mut add = serde_json::to_value(op(Op::AddDef {
        id,
        parent: None,
        ordinal: 0,
        definition: "fn added() {}".into(),
        intent: Intent::Feature,
        file: Some(RelPath::new("src/lib.rs").unwrap()),
    }))
    .unwrap();
    add["subject"] = serde_json::json!({
        "id": id,
        "before_name": null,
        "after_name": "added",
        "kind": "Fn",
        "file": "src/lib.rs",
        "before_source": null,
        "after_source": "fn added() {}",
        "touch": "Added"
    });
    let add: OperationView = serde_json::from_value(add).unwrap();
    assert_eq!(add.subject.as_ref().unwrap().before_name, "");
    assert_eq!(add.subject.as_ref().unwrap().before_source, "");

    let mut delete = serde_json::to_value(op(Op::Delete {
        id,
        intent: Intent::Refactor,
    }))
    .unwrap();
    delete["subject"] = serde_json::json!({
        "id": id,
        "before_name": "added",
        "after_name": null,
        "kind": "Fn",
        "file": "src/lib.rs",
        "before_source": "fn added() {}",
        "after_source": null,
        "touch": "Removed"
    });
    let delete: OperationView = serde_json::from_value(delete).unwrap();
    assert_eq!(delete.subject.as_ref().unwrap().after_name, "");
    assert_eq!(delete.subject.as_ref().unwrap().after_source, "");
}

#[tokio::test]
async fn browser_contract_has_typed_labels_change_navigation_and_entity_filters() {
    let (status, html) = response(svc_forge::app(catalog()), "/").await;
    assert_eq!(status, StatusCode::OK);
    for contract in [
        "semantic development history",
        "word==='entity'?'entities'",
        "buildChanges",
        "repo.heads?.length",
        "groupSummary",
        "repositoryDescription",
        "Semantic history recorded by svc.",
        "renderChangeList",
        "renderChangeDetail",
        "operationIx",
        "undoTarget",
        "Undid #${operationIx(target.entry,target.index)}",
        "Renamed ${before} → ${after}",
        "Typed operations",
        "sourceDiff",
        "syntax-keyword",
        "Current source",
        "Entity history",
        "Review queue",
        "entity-query",
        "entity-kind",
        "include-synthetic",
        "PAGE_SIZE=100",
        "B:'merged-in change'",
        "Recorded the semantic resolution for replay.",
        "from checkout ${entry.workspace}",
        "Absorbed hand edits: ",
        "s.entity===entity.id",
        "checkouts",
        "What changed, entity by entity",
        "function lineDiff(a,b)",
        "/subjects`",
        "filteredEntities",
        "conflictSummary",
        "Delete/edit conflict",
        "selectEntity",
        "Parent reference",
        "Content identity",
        "Technical record",
        "@media(max-width:900px)",
        "overflow-wrap:anywhere",
        "word-break:break-word",
    ] {
        assert!(
            html.contains(contract),
            "missing browser contract: {contract}"
        );
    }
}

#[test]
fn example_catalog_tracks_the_dogfood_repository() {
    let parsed: Catalog = serde_json::from_str(include_str!("../examples/forge.json")).unwrap();
    assert_eq!(parsed.repositories[0].slug, "svc");
    assert_eq!(parsed.repositories[0].snapshots[0].id, "demo-head");
}

#[tokio::test]
async fn file_source_merges_two_catalogs() {
    let dir = TestDir::new();
    let alpha = dir.path().join("alpha.json");
    let beta = dir.path().join("beta.json");
    write_forge_catalog(&alpha, "alpha", "alpha-head");
    write_forge_catalog(&beta, "beta", "beta-head");

    let app = svc_forge::app_from_paths(vec![alpha, beta]).unwrap();
    let (_, text) = response(app.clone(), "/api/repositories").await;
    let repositories: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(repositories.as_array().unwrap().len(), 2);
    assert_eq!(repositories[0]["slug"], "alpha");
    assert_eq!(repositories[1]["slug"], "beta");

    let (status, text) = response(app, "/api/repositories/beta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["head"],
        "beta-head"
    );
}

#[test]
fn file_source_rejects_duplicate_slugs() {
    let dir = TestDir::new();
    let first = dir.path().join("first.json");
    let second = dir.path().join("second.json");
    write_forge_catalog(&first, "same", "first-head");
    write_forge_catalog(&second, "same", "second-head");

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
    write_forge_catalog(&live, "svc", "old-head");
    let app = svc_forge::app_from_paths(vec![live.clone()]).unwrap();

    let (_, before) = response(app.clone(), "/api/repositories/svc").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&before).unwrap()["head"],
        "old-head"
    );

    write_forge_catalog(&replacement, "svc", "new-head");
    std::fs::rename(&replacement, &live).unwrap();
    let (_, after) = response(app, "/api/repositories/svc").await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&after).unwrap()["head"],
        "new-head"
    );
}
