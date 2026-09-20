//! `svc forge export` (done-criterion 1): the catalog `crates/svc-forge` serves, produced
//! from this store so the browser shows *this* repository rather than a fixture. The shape
//! is `svc_forge::Catalog`'s serde form, written here as JSON so `svc-repo` needs no
//! dependency on the web crate; operations, conflicts and review items are the `svc-core`
//! types the forge deserialises. Heads and the root carry their entities; ancestors are
//! listed bare so a long history on a large crate stays a small file. The forge reads the
//! file, never the store, so it can never contend with a writer.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use svc_core::engine::render_entity;
use svc_core::{Conflict, EntityId, Error, Op, OpIx, OpLogEntry, RelPath, ReviewItem, Result, Snapshot, SnapshotId, Store};

use crate::history::{op_entity, touch, touches};
use crate::repo::Repo;

/// How many ancestor snapshots to list beyond the heads and the root.
const ANCESTOR_LIMIT: usize = 200;

pub fn catalog(repo: &Repo) -> Result<Value> {
    let store = repo.store();
    let view = repo.view()?;
    let mut full: BTreeSet<SnapshotId> = view.heads.values().copied().collect();
    full.insert(view.root);

    // Breadth-first over parents and predecessors from every head and the root.
    let mut order: Vec<SnapshotId> = Vec::new();
    let mut seen: BTreeSet<SnapshotId> = BTreeSet::new();
    // Root first, then the other heads, then ancestors.
    let mut queue: VecDeque<SnapshotId> = VecDeque::from([view.root]);
    queue.extend(full.iter().copied().filter(|id| *id != view.root));
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        order.push(id);
        if order.len() > full.len() + ANCESTOR_LIMIT {
            break;
        }
        if let Ok(s) = store.get_snapshot(id) {
            queue.extend(s.parents.iter().chain(s.predecessors.iter()).copied());
        }
    }

    let mut snapshots = Vec::with_capacity(order.len());
    for id in &order {
        let s = store.get_snapshot(*id)?;
        let entities: Vec<Value> = if full.contains(id) {
            s.entities
                .iter()
                .map(|(eid, rec)| {
                    let mut v = serde_json::to_value(rec).unwrap_or(Value::Null);
                    if let Value::Object(m) = &mut v {
                        m.insert("id".into(), json!(eid));
                        if let Some(text) = source(store, &s, *eid) {
                            m.insert("source".into(), Value::String(text));
                        }
                    }
                    v
                })
                .collect()
        } else {
            Vec::new()
        };
        snapshots.push(json!({
            "id": id,
            "change": s.change,
            "message": s.message,
            "parents": s.parents,
            "entities": entities,
            "conflicts": s.conflicts,
        }));
    }

    let ops = store.ops(OpIx(0), false)?;
    let operations: Vec<Value> = ops
        .iter()
        .map(|(ix, e)| {
            let ws = repo.redb().op_workspace(*ix).ok().flatten().filter(|w| !w.is_empty());
            operation(store, *ix, e, ws.as_deref())
        })
        .collect();

    // The review queue as the review rules define it: every edit-def, and every binding conflict
    // in the current snapshot. (Changesets' own `queue` is not populated by any verb.)
    let mut review: Vec<ReviewItem> = ops
        .iter()
        .filter_map(|(ix, e)| match &e.op {
            Op::EditDef { intent, .. } => Some(ReviewItem::EditReview {
                op: *ix,
                declared: intent.clone(),
                observed: e.observed,
                ask_id: None,
            }),
            _ => None,
        })
        .collect();
    let cur = repo.current()?;
    review.extend(cur.conflicts.iter().filter(|c| matches!(c, Conflict::Binding { .. })).map(|c| ReviewItem::BindingConflict { conflict: c.clone() }));

    // What every checkout is on: this one's root plus the named workspaces' — the
    // forge marks those changes current, not every change that still has a head.
    let mut checkouts = vec![view.root];
    checkouts.extend(repo.redb().workspaces()?.into_iter().filter_map(|(_, w)| w.root));
    let root = repo.root_dir();
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    Ok(json!({
        "repositories": [{
            "slug": slug,
            "name": name,
            "path": root,
            "description": format!("{} entities, {} operations, {} changes", cur.entities.len(), ops.len(), view.heads.len()),
            "head": view.root,
            "heads": checkouts,
            "snapshots": snapshots,
            "operations": operations,
            "review_queue": review,
        }]
    }))
}

/// The entity's rendered text, for the browser's entity and change pages.
fn source(store: &dyn Store, snap: &Snapshot, id: EntityId) -> Option<String> {
    render_entity(snap, store, id, false)
        .ok()
        .map(|(bytes, _)| String::from_utf8_lossy(&bytes).into_owned())
}

/// The raw log entry plus what the browser needs beside it: `ix`, the after-root's `change`,
/// and for an op about one entity a `subject` with its names, kind, file, both sources and
/// the touch — so a change page can show a rename or edit without opening the store.
fn operation(store: &dyn Store, ix: OpIx, e: &OpLogEntry, workspace: Option<&str>) -> Value {
    let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
    let Value::Object(m) = &mut v else { return v };
    m.insert("ix".into(), json!(ix));
    let after = store.get_snapshot(e.after.root).ok();
    if let Some(after) = &after {
        m.insert("change".into(), json!(after.change));
    }
    if let Some(name) = e.group.and_then(|g| store.get_changeset(g).ok()).map(|cs| cs.name) {
        m.insert("group_name".into(), json!(name));
    }
    if let Some(w) = workspace {
        m.insert("workspace".into(), json!(w));
    }
    // Every entity the op touched and every opaque file it changed, so an absorb or a
    // merge — the ops that carry other people's work — has names and paths too.
    let before = store.get_snapshot(e.before.root).ok();
    if let (Some(b), Some(a)) = (&before, &after) {
        let touched = touches(b, a, e.observed);
        if !touched.is_empty() {
            // Each touch carries the entity's text on both sides when the op changed it, so
            // a change page can show an absorb entity by entity. `New` is the init op with
            // everything "added": its texts are the snapshot's, not a change.
            let with_text = !matches!(e.op, Op::New { .. } | Op::Describe { .. } | Op::Branch { .. });
            let subjects: Vec<Value> = touched
                .iter()
                .map(|t| {
                    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
                    let Value::Object(sm) = &mut v else { return v };
                    let prev = b.entities.get(&t.entity);
                    let next = a.entities.get(&t.entity);
                    if let Some(r) = next.or(prev) {
                        sm.insert("kind".into(), json!(r.kind));
                        sm.insert("file".into(), json!(r.file));
                    }
                    if with_text {
                        let before_text = prev.and_then(|_| source(store, b, t.entity));
                        let after_text = next.and_then(|_| source(store, a, t.entity));
                        if before_text != after_text {
                            if let Some(x) = before_text {
                                sm.insert("before_source".into(), Value::String(x));
                            }
                            if let Some(x) = after_text {
                                sm.insert("after_source".into(), Value::String(x));
                            }
                        }
                    }
                    v
                })
                .collect();
            m.insert("subjects".into(), Value::Array(subjects));
        }
        let changed: Vec<&RelPath> = a
            .files
            .iter()
            .filter(|(p, rec)| b.files.get(*p) != Some(rec))
            .map(|(p, _)| p)
            .chain(b.files.keys().filter(|p| !a.files.contains_key(*p)))
            .collect();
        if !changed.is_empty() {
            m.insert("files".into(), json!(changed));
        }
    }
    if let (Some(id), Some(after)) = (op_entity(&e.op), &after) {
        let prev = before.as_ref().and_then(|b| b.entities.get(&id));
        let next = after.entities.get(&id);
        let shown = next.or(prev);
        // Absent rather than null: the browser's fields default when a key is missing.
        let mut subject = serde_json::Map::new();
        subject.insert("id".into(), json!(id));
        let mut put = |key: &str, value: Option<Value>| {
            if let Some(v) = value {
                subject.insert(key.into(), v);
            }
        };
        put("before_name", prev.map(|r| json!(r.name)));
        put("after_name", next.map(|r| json!(r.name)));
        put("kind", shown.map(|r| json!(r.kind)));
        put("file", shown.map(|r| json!(r.file)));
        put("before_source", before.as_ref().and_then(|b| source(store, b, id)).map(Value::String));
        put("after_source", source(store, after, id).map(Value::String));
        put("touch", touch(prev, next, e.observed).map(|t| json!(t)));
        m.insert("subject".into(), Value::Object(subject));
    }
    v
}

/// Write the catalog; default `.svc/forge.json` under the repository root.
pub fn export(repo: &Repo, out: Option<&Path>) -> Result<PathBuf> {
    let path = out.map(Path::to_path_buf).unwrap_or_else(|| repo.root_dir().join(".svc/forge.json"));
    let text = serde_json::to_string_pretty(&catalog(repo)?).map_err(|e| Error::Other(e.to_string()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error::Other(e.to_string()))?;
    }
    // Atomic replace: the forge may be reading the old file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| Error::Other(e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| Error::Other(e.to_string()))?;
    Ok(path)
}
