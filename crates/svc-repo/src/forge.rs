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
use svc_core::{Conflict, Error, Op, OpIx, ReviewItem, Result, SnapshotId};

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
    let operations: Vec<Value> = ops.iter().map(|(_, e)| serde_json::to_value(e).unwrap_or(Value::Null)).collect();

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
            "snapshots": snapshots,
            "operations": operations,
            "review_queue": review,
        }]
    }))
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
