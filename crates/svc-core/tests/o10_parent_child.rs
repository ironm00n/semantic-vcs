//! SPEC §10b, O10 — parent/child invariant.
//!
//! "for every entity `e` with `parent = Some(p)`, `p`'s chunk list and content
//! stream each contain exactly one `Child(e)`, and conversely; no orphans."
//!
//! Built only against the frozen top-level `svc_core::engine::*` functions
//! (`parse`, `extract`, `resolve`, `canonicalize`, `to_bytes`) so it should
//! not need edits as engine internals move underneath it.

use std::collections::{BTreeMap, BTreeSet};

use svc_core::content::{Chunk, Token};
use svc_core::engine::{canonicalize, extract, parse, resolve, to_bytes};
use svc_core::ids::{ByteRange, EntityId};
use svc_core::lang::{Env, Lang};
use svc_core::RustLang;

/// Find the tree-sitter node whose span exactly matches `range`, searching
/// from `root` down. `extract()` derives `item_range` from a real node span,
/// so an exact match always exists for a well-formed tree.
fn find_node<'t>(root: tree_sitter::Node<'t>, range: ByteRange) -> Option<tree_sitter::Node<'t>> {
    if root.start_byte() as u32 == range.start && root.end_byte() as u32 == range.end {
        return Some(root);
    }
    if (root.start_byte() as u32) > range.start || (root.end_byte() as u32) < range.end {
        return None;
    }
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if let Some(found) = find_node(child, range) {
            return Some(found);
        }
    }
    None
}

fn check_parent_child(src: &str, lang: &dyn Lang) {
    let tree = parse(src.as_bytes(), lang).unwrap();
    let raw = extract(&tree, src.as_bytes(), lang).unwrap();
    let ids: Vec<EntityId> = (0..raw.len()).map(|_| EntityId::new()).collect();
    let env = Env::default();

    // What extract() itself claims the parent/child relation is.
    let mut expected: BTreeMap<usize, BTreeSet<EntityId>> = BTreeMap::new();
    for (i, ent) in raw.iter().enumerate() {
        if let Some(p) = ent.parent_idx {
            expected.entry(p).or_default().insert(ids[i]);
        }
    }

    let mut seen_as_child_anywhere: BTreeSet<EntityId> = BTreeSet::new();

    for (i, ent) in raw.iter().enumerate() {
        let node = find_node(tree.root_node(), ent.item_range)
            .unwrap_or_else(|| panic!("no node for entity {:?} ({})", ent.item_range, ent.name));
        let res = resolve(node, src.as_bytes(), lang, &env).unwrap();

        // bytes_from_span's own convention (mirrors engine::ingest_file): children keyed by
        // bytes_range (trivia-inclusive).
        let bytes_children: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].bytes_range, ids[c]))
            .collect();
        // canonicalize's own convention (mirrors materialize()): children keyed by item_range.
        let content_children: Vec<(ByteRange, EntityId)> = ent
            .children
            .iter()
            .map(|&c| (raw[c].item_range, ids[c]))
            .collect();

        let bytes = to_bytes(node, src.as_bytes(), &res, &bytes_children, None).unwrap();
        let content =
            canonicalize(node, src.as_bytes(), &res, &content_children, &env, lang).unwrap();

        let want: BTreeSet<EntityId> = expected.remove(&i).unwrap_or_default();
        for id in &want {
            seen_as_child_anywhere.insert(*id);
        }

        let mut in_bytes: BTreeMap<EntityId, u32> = BTreeMap::new();
        for c in bytes.chunks() {
            if let Chunk::Child(id) = c {
                *in_bytes.entry(*id).or_insert(0) += 1;
            }
        }
        let bytes_ids: BTreeSet<EntityId> = in_bytes.keys().copied().collect();
        assert_eq!(
            bytes_ids, want,
            "Bytes::chunks() Child holes for entity `{}` do not match extract()'s children",
            ent.name
        );
        assert!(
            in_bytes.values().all(|&n| n == 1),
            "duplicate Chunk::Child hole in bytes for entity `{}`",
            ent.name
        );

        let mut in_content: BTreeMap<EntityId, u32> = BTreeMap::new();
        for t in &content.tokens {
            if let Token::Child(id) = t {
                *in_content.entry(*id).or_insert(0) += 1;
            }
        }
        let content_ids: BTreeSet<EntityId> = in_content.keys().copied().collect();
        assert_eq!(
            content_ids, want,
            "Content.tokens Child tokens for entity `{}` do not match extract()'s children",
            ent.name
        );
        assert!(
            in_content.values().all(|&n| n == 1),
            "duplicate Token::Child in content for entity `{}`",
            ent.name
        );
    }

    assert!(
        expected.is_empty(),
        "some parents named by extract() were never visited: {:?}",
        expected.keys().collect::<Vec<_>>()
    );

    // No orphans: every entity extract() calls a child must have actually been asserted as
    // wanted by exactly one parent above (BTreeMap/BTreeSet dedup already enforces "at most
    // one"; this checks "at least one" for every non-root entity).
    for (i, ent) in raw.iter().enumerate() {
        if ent.parent_idx.is_some() {
            assert!(
                seen_as_child_anywhere.contains(&ids[i]),
                "entity `{}` claims a parent but was never found as a Child hole",
                ent.name
            );
        }
    }
}

#[test]
fn o10_simple_file_no_nesting() {
    let src = "struct Config { path: String }\n\nfn read(path: &str) -> String {\n    path.to_string()\n}\n";
    check_parent_child(src, &RustLang);
}

#[test]
fn o10_impl_with_methods() {
    let src = r#"struct Config;

impl Config {
    fn new() -> Self {
        Config
    }

    fn other(&self) -> u32 {
        0
    }
}
"#;
    check_parent_child(src, &RustLang);
}

#[test]
fn o10_deeply_nested_fn() {
    let src = r#"fn outer() {
    fn inner() {
        fn innermost() -> u32 { 0 }
    }
}
"#;
    check_parent_child(src, &RustLang);
}

#[test]
fn o10_svc_core_source() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    visit(&root, &mut files);
    assert!(!files.is_empty());
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        check_parent_child(&src, &RustLang);
    }
}

fn visit(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let p = e.path();
        if p.is_dir() {
            visit(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}
