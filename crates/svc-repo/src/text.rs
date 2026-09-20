//! Human-readable renderings of the verb outputs (`--json` is the machine form;
//! without it the expo reads sentences). Entity references print as `name⟨short⟩`, never
//! a bare UUID.

use svc_core::{Conflict, EntityId, IdentRef, Intent, ObservedClass, Op, Snapshot};

use crate::history::{BlameEntry, ChangeOut, EvologEntry, OpOut, StatusOut, Touch};
use crate::merge::{ConflictOut, MergeOut};

pub fn entity_ref(snap: &Snapshot, id: EntityId) -> String {
    match snap.entities.get(&id) {
        Some(r) => format!("{}⟨{}⟩", r.name, id.short()),
        None => format!("?⟨{}⟩", id.short()),
    }
}

pub fn intent(i: &Intent) -> String {
    match i {
        Intent::Other(s) => s.clone(),
        other => format!("{other:?}").to_lowercase(),
    }
}

pub fn class(c: Option<ObservedClass>) -> &'static str {
    match c {
        Some(ObservedClass::Alpha) => "alpha",
        Some(ObservedClass::DocsOnly) => "docs-only",
        Some(ObservedClass::BindingPreserving) => "binding-preserving",
        Some(ObservedClass::BindingChanging) => "binding-changing",
        None => "unclassified",
    }
}

/// One event line: `renamed parse → parse_config`, `edit-def validate (declared refactor,
/// observed binding-changing) ✗`.
pub fn op(snap: &Snapshot, e: &OpOut) -> String {
    let verdict = match (e.declared.as_ref(), e.observed) {
        (Some(d), Some(o)) => format!(
            " (declared {}, observed {}) {}",
            intent(d),
            class(Some(o)),
            if e.flagged { "✗ flagged" } else { "✓" }
        ),
        (Some(d), None) => format!(" (declared {}, unchecked)", intent(d)),
        (None, Some(o)) => format!(" (observed {})", class(Some(o))),
        (None, None) => String::new(),
    };
    // Prefer the name recorded with the op; the current snapshot may no longer hold it.
    let subj = |id: EntityId| match &e.subject {
        Some(n) => format!("{n}⟨{}⟩", id.short()),
        None => entity_ref(snap, id),
    };
    let body = match &e.op {
        Op::Rename { id, new } => match &e.subject {
            Some(old) => format!("renamed {old} → {new}"),
            None => format!("renamed {} → {new}", name_before(snap, *id, new)),
        },
        Op::Move { id, parent, .. } => format!(
            "moved {} → {}",
            subj(*id),
            parent.map(|p| entity_ref(snap, p)).unwrap_or_else(|| "top level".into())
        ),
        Op::Relocate { id, file, ordinal } => format!("relocated {} → {file}#{ordinal}", subj(*id)),
        Op::Extract { id, .. } => format!("extracted {}", subj(*id)),
        Op::Inline { id } => format!("inlined {}", subj(*id)),
        Op::AddDef { id, .. } => format!("add-def {}", subj(*id)),
        Op::Delete { id, .. } => format!("deleted {}", subj(*id)),
        Op::EditDef { id, .. } => format!("edit-def {}", subj(*id)),
        Op::Merge { other } => format!("merged change {}", other.short()),
        Op::Undo => "undo".into(),
        Op::New { change } => format!("new change {}", change.short()),
        Op::Describe { msg } => format!("described: {msg:?}"),
        Op::Branch { name } => format!("branch {name}"),
        Op::Absorb => "absorbed hand edits".into(),
        Op::Resolve { conflict, take } => format!("resolved conflict {conflict}: took {take:?}"),
    };
    format!("#{:<3} {body}{verdict}", e.ix.0)
}

fn name_before(snap: &Snapshot, id: EntityId, new: &str) -> String {
    // The snapshot is the current one, so the entity already carries `new`; show the id
    // so the line still identifies it if it has been renamed again since.
    match snap.entities.get(&id) {
        Some(r) if r.name != new => format!("{}⟨{}⟩", r.name, id.short()),
        _ => format!("⟨{}⟩", id.short()),
    }
}

pub fn log(snap: &Snapshot, entries: &[OpOut]) -> String {
    if entries.is_empty() {
        return "no events".into();
    }
    entries.iter().map(|e| op(snap, e)).collect::<Vec<_>>().join("\n")
}

pub fn heads(heads: &[ChangeOut]) -> String {
    heads
        .iter()
        .map(|h| {
            format!(
                "{} {} {}{}",
                if h.current { "@" } else { " " },
                h.short,
                if h.branches.is_empty() { String::new() } else { format!("[{}] ", h.branches.join(", ")) },
                if h.message.is_empty() { "(no description)".to_string() } else { h.message.clone() }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn touch(t: &Touch) -> String {
    match t {
        Touch::Added => "added".into(),
        Touch::Removed => "removed".into(),
        Touch::Renamed { from, to } => format!("renamed {from} → {to}"),
        Touch::Moved { .. } => "moved".into(),
        Touch::Relocated { from, to } => format!("relocated {}#{} → {}#{}", from.0, from.1, to.0, to.1),
        Touch::Edited { observed } => format!("edited ({})", class(*observed)),
    }
}

pub fn evolog(entries: &[EvologEntry]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let head = format!(
                "v{} {} {} ({} entities)",
                entries.len() - i,
                e.snapshot.short(),
                if e.message.is_empty() { "(no description)".to_string() } else { e.message.clone() },
                e.entities
            );
            let deltas: Vec<String> = e
                .deltas
                .iter()
                .map(|d| format!("    {} {}", d.name, touch(&d.touch)))
                .collect();
            if deltas.is_empty() {
                head
            } else {
                format!("{head}\n{}", deltas.join("\n"))
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn blame(snap: &Snapshot, entries: &[BlameEntry]) -> String {
    entries
        .iter()
        .map(|b| {
            let what = match &b.op {
                Op::Rename { .. } | Op::New { .. } | Op::Absorb => String::new(),
                other => format!(" — {}", op_verb(snap, other)),
            };
            format!("#{:<3} change {} {}{}", b.ix.0, b.change.short(), touch(&b.touch), what)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn op_verb(snap: &Snapshot, o: &Op) -> String {
    op(
        snap,
        &OpOut {
            ix: svc_core::OpIx(0),
            op: o.clone(),
            declared: o.intent().cloned(),
            observed: None,
            flagged: false,
            at: 0,
            group: None,
            root_after: snap.id(),
            subject: None,
            workspace: None,
        },
    )
    .trim_start_matches(|c: char| c == '#' || c.is_ascii_digit() || c == ' ')
    .to_string()
}

pub fn status(snap: &Snapshot, s: &StatusOut) -> String {
    let mut lines = vec![match s.conflicts {
        0 => s.summary.clone(),
        1 => format!("{}; 1 conflict — svc conflicts", s.summary),
        n => format!("{}; {n} conflicts — svc conflicts", s.summary),
    }];
    for d in &s.deltas {
        lines.push(format!("    {}", delta(snap, d)));
    }
    lines.join("\n")
}

pub fn delta(snap: &Snapshot, d: &svc_core::Delta) -> String {
    use svc_core::Delta;
    // Alpha edits are the layout-only line of demo line 2: bytes differ, content does not.
    match d {
        Delta::Added(id) => format!("added {}", entity_ref(snap, *id)),
        Delta::Removed { id, name } => format!("removed {name}⟨{}⟩", id.short()),
        Delta::Renamed { from, to, .. } => format!("renamed {from} → {to}"),
        Delta::Moved { id, .. } => format!("moved {}", entity_ref(snap, *id)),
        Delta::Relocated { id, from, to } => format!("relocated {} {}#{} → {}#{}", entity_ref(snap, *id), from.0, from.1, to.0, to.1),
        Delta::Edited(id, ObservedClass::Alpha) => format!("{} edited: alpha (local renamed; content hash unchanged)", entity_ref(snap, *id)),
        Delta::Edited(id, c) => format!("{} edited: {}", entity_ref(snap, *id), class(Some(*c))),
        Delta::FileAdded(p) => format!("added file {p}"),
        Delta::FileRemoved(p) => format!("removed file {p}"),
        Delta::FileTail { path, whitespace_only: true } => format!("{path}: whitespace outside entities changed"),
        Delta::FileTail { path, .. } => format!("{path}: bytes outside entities changed"),
    }
}

fn ident(snap: &Snapshot, r: &IdentRef) -> String {
    match r {
        IdentRef::Local(slot, _) => format!("local ${}", slot.0),
        IdentRef::Entity(id) if *id == EntityId::SELF => "itself".into(),
        IdentRef::Entity(id) => entity_ref(snap, *id),
        IdentRef::Free(n) => format!("free `{n}`"),
    }
}

pub fn conflict(snap: &Snapshot, c: &ConflictOut) -> String {
    let body = match &c.conflict {
        Conflict::Binding { name, at, was, was_at, now, now_at, .. } => format!(
            "binding conflict in {}: `{name}` at {}:{} meant {}{}, now means {}{}",
            c.name,
            at.line,
            at.col,
            ident(snap, was),
            was_at.map(|p| format!(" (declared {}:{})", p.line, p.col)).unwrap_or_default(),
            ident(snap, now),
            now_at.map(|p| format!(" (declared {}:{})", p.line, p.col)).unwrap_or_default(),
        ),
        Conflict::Attr { sides, .. } => {
            let vals: Vec<String> = sides.adds().map(|v| format!("{v:?}")).collect();
            format!("attribute conflict on {}: {}", c.name, vals.join(" vs "))
        }
        Conflict::Content { hunks, .. } => format!("content conflict on {} ({} hunk(s))", c.name, hunks.len().max(1)),
        Conflict::AddAdd { key, .. } => format!("add/add: both sides added `{}`", key.name),
        Conflict::DeleteEdit { deleted_by, edited_by, .. } => {
            format!("delete/edit on {}: deleted by {deleted_by:?}, edited by {edited_by:?}", c.name)
        }
    };
    format!("[{}] {body}", c.n)
}

pub fn conflicts(snap: &Snapshot, cs: &[ConflictOut]) -> String {
    if cs.is_empty() {
        return "no conflicts".into();
    }
    cs.iter().map(|c| conflict(snap, c)).collect::<Vec<_>>().join("\n")
}

pub fn merge(snap: &Snapshot, store: &dyn svc_core::Store, root: &std::path::Path, m: &MergeOut) -> String {
    let mut lines = vec![format!(
        "merged into change {} (snapshot {}): {}",
        m.change.short(),
        m.snapshot.short(),
        if m.conflicts.is_empty() { "clean".to_string() } else { format!("{} conflict(s)", m.conflicts.len()) }
    )];
    for (b, a) in &m.unified {
        lines.push(format!("    unified {} into {}", b.short(), a.short()));
    }
    for c in &m.conflicts {
        lines.push(format!("    {}", conflict_named(snap, store, root, c)));
    }
    lines.join("\n")
}

/// A binding conflict the way a reader needs it: the identifier, the use, and both binders,
/// recovered from the rendered entity and its ident map — the record carries the slots
/// (reliable) and positions (not yet), and the expo line is "`raw` at line 8 meant the
/// `let raw` at line 2, now means the `let raw` at line 3".
pub fn conflict_named(snap: &Snapshot, store: &dyn svc_core::Store, root: &std::path::Path, c: &ConflictOut) -> String {
    let Conflict::Binding { id, was, now, at, .. } = &c.conflict else {
        return conflict(snap, c);
    };
    let Ok((src, Some(map))) = svc_core::engine::render_entity(snap, store, *id, true) else {
        return conflict(snap, c);
    };
    let text = String::from_utf8_lossy(&src).into_owned();
    // Positions as the reader sees them: file line numbers when the rendered entity is
    // found verbatim in the working copy, else lines within the item (the rendered bytes
    // carry the blank lines that precede it; do not count those).
    let file = snap.entities.get(id).map(|r| r.file.clone());
    let base = file
        .as_ref()
        .and_then(|f| std::fs::read_to_string(root.join(f.as_str())).ok())
        .and_then(|whole| whole.find(text.trim_start_matches('\n')).map(|i| whole[..i].matches('\n').count()));
    let leading = text.len() - text.trim_start_matches('\n').len();
    let where_ = |line: usize| match (&file, base) {
        (Some(f), Some(b)) => format!("{}:{}", f.as_str(), b + line - leading),
        _ => format!("line {}", line - leading),
    };
    let line_of = |off: usize| text[..off.min(text.len())].matches('\n').count() + 1;
    let occurrences = |r: &IdentRef| -> Vec<(usize, String)> {
        let mut v: Vec<(usize, String)> = map
            .iter()
            .filter(|(_, ident)| ident == r)
            .map(|(range, _)| (range.start as usize, text[range.start as usize..range.end as usize].to_string()))
            .collect();
        v.sort();
        v
    };
    let (now_occ, was_occ) = (occurrences(now), occurrences(was));
    let (Some((now_decl, name)), Some((was_decl, was_name))) = (now_occ.first(), was_occ.first()) else {
        return conflict(snap, c);
    };
    let binder = |off: usize, name: &str| {
        let line = line_of(off);
        let start = text[..off].rfind('\n').map_or(0, |i| i + 1);
        if text[start..off].trim_start().starts_with("let ") {
            format!("the `let {name}` at {}", where_(line))
        } else {
            format!("`{name}` bound at {}", where_(line))
        }
    };
    // The use: the record's position when it lands on this identifier, else its last use.
    let use_at = now_occ
        .iter()
        .find(|(off, _)| line_of(*off) == at.line as usize)
        .or(now_occ.last())
        .map(|(off, _)| line_of(*off))
        .unwrap_or(at.line as usize);
    format!(
        "[{}] binding conflict in {}: `{name}` at {} meant {}, now means {}{}",
        c.n,
        c.name,
        where_(use_at),
        binder(*was_decl, was_name),
        binder(*now_decl, name),
        if was_name == name { " (shadowed)" } else { "" }
    )
}

pub fn conflicts_named(snap: &Snapshot, store: &dyn svc_core::Store, root: &std::path::Path, cs: &[ConflictOut]) -> String {
    if cs.is_empty() {
        return "no conflicts".into();
    }
    cs.iter().map(|c| conflict_named(snap, store, root, c)).collect::<Vec<_>>().join("\n")
}
