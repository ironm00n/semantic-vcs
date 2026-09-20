//! The store writes `Op` with postcard, which encodes an enum variant as its index and
//! nothing else. Inserting a variant anywhere but at the end re-labels every op already
//! in every store: `Restore` added after `Undo` made a stored `New` read back as `Restore`
//! and the ops after it fail to decode ("Option discriminant that wasn't 0 or 1").
//! This table is the wire format. Add new variants at the end and extend the table.
use svc_core::ids::{ChangeId, ChangeSetId, EntityId, RelPath};
use svc_core::{Intent, NoteKind, NoteTo, Op, Take};

fn index_of(op: &Op) -> u8 {
    postcard::to_allocvec(op).unwrap()[0]
}

#[test]
fn op_variant_indices_are_append_only() {
    let id = EntityId::new();
    let table: [(Op, u8); 17] = [
        (Op::Rename { id, new: "n".into() }, 0),
        (Op::Move { id, parent: None, ordinal: None }, 1),
        (Op::Relocate { id, file: RelPath::new("a.rs").unwrap(), ordinal: 0 }, 2),
        (Op::Extract { id, new_parent: None, ordinal: 0 }, 3),
        (Op::Inline { id }, 4),
        (
            Op::AddDef { id, parent: None, file: None, ordinal: 0, definition: String::new(), intent: Intent::Fix },
            5,
        ),
        (Op::Delete { id, intent: Intent::Fix }, 6),
        (Op::EditDef { id, definition: String::new(), intent: Intent::Fix }, 7),
        (Op::Merge { other: ChangeId::new() }, 8),
        (Op::Undo, 9),
        (Op::New { change: ChangeId::new() }, 10),
        (Op::Describe { msg: String::new() }, 11),
        (Op::Branch { name: String::new() }, 12),
        (Op::Absorb, 13),
        (Op::Resolve { conflict: 0, take: Take::A }, 14),
        (Op::Restore { at: 0 }, 15),
        (
            Op::Note {
                to: NoteTo::All,
                kind: NoteKind::Note,
                text: String::new(),
            },
            16,
        ),
    ];
    for (op, want) in &table {
        assert_eq!(index_of(op), *want, "{op:?} moved on the wire");
    }
}

#[test]
fn intent_and_take_indices_are_append_only() {
    for (i, intent) in [Intent::Refactor, Intent::Fix, Intent::Feature, Intent::Docs, Intent::Other(String::new())]
        .iter()
        .enumerate()
    {
        assert_eq!(postcard::to_allocvec(intent).unwrap()[0] as usize, i, "{intent:?}");
    }
    for (i, take) in [Take::A, Take::B, Take::Base].iter().enumerate() {
        assert_eq!(postcard::to_allocvec(take).unwrap()[0] as usize, i, "{take:?}");
    }
    for (i, to) in [
        NoteTo::Checkout(String::new()),
        NoteTo::All,
        NoteTo::Changeset(ChangeSetId::new()),
        NoteTo::Entity(EntityId::new()),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(postcard::to_allocvec(to).unwrap()[0] as usize, i, "{to:?}");
    }
    for (i, kind) in [
        NoteKind::Note,
        NoteKind::Approve,
        NoteKind::RequestChanges,
        NoteKind::Claim,
        NoteKind::Release,
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(postcard::to_allocvec(kind).unwrap()[0] as usize, i, "{kind:?}");
    }
}
