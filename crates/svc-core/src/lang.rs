use std::collections::HashMap;

use crate::content::Namespace;
use crate::entity::Kind;
use crate::ids::{ByteRange, EntityId, RelPath};

#[derive(Clone, Debug, Default)]
pub struct Env {
    pub names: HashMap<(String, Namespace), EntityId>,
}

impl Env {
    pub fn lookup(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        self.names
            .get(&(name.to_string(), ns))
            .copied()
            .or_else(|| self.names.get(&(name.to_string(), Namespace::Any)).copied())
    }

    pub fn insert(&mut self, name: impl Into<String>, ns: Namespace, id: EntityId) {
        self.names.insert((name.into(), ns), id);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EntityKindRule {
    pub node_kind: &'static str,
    pub kind: Kind,
    pub name_field: Option<&'static str>,
    pub body_field: Option<&'static str>,
    pub children_field: Option<&'static str>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Locator {
    Itself,
    Field(&'static str),
    ChildIndex(usize),
    FieldWithKind(&'static str, &'static str),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Whole,
    AfterStmt,
    /// Fields resolve against the binder's own parent, not the scope node.
    Sub(&'static [&'static str]),
    Chain,
    Hoisted,
    Inherit,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinderClass {
    Local,
    Generic,
    Label,
    Item,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum When {
    Always,
    ThroughBlock,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Barrier {
    pub ns: Namespace,
    pub class: BinderClass,
    pub when: When,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Binder {
        namespace: Namespace,
        visibility: Visibility,
        locator: Locator,
    },
    Reference {
        namespace: Namespace,
    },
    Scope {
        opens: &'static [Namespace],
        barriers: &'static [Barrier],
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CommutativeRule {
    pub parent: Option<Kind>,
    pub only_child_kinds: Option<&'static [Kind]>,
    pub except_child_kinds: Option<&'static [Kind]>,
}

pub trait Lang: Send + Sync {
    fn language(&self) -> tree_sitter::Language;
    fn name(&self) -> &'static str;
    fn extensions(&self) -> &'static [&'static str];
    fn entity_kinds(&self) -> &'static [EntityKindRule];
    fn entity_name(&self, node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String>;
    fn roles(
        &self,
        node: tree_sitter::Node<'_>,
        field: Option<&str>,
        src: &[u8],
        env: &Env,
    ) -> Vec<Role>;
    fn opaque_nodes(&self) -> &'static [&'static str];
    fn commutative_parents(&self) -> &'static [CommutativeRule];
    fn trivia_kinds(&self) -> &'static [&'static str];
}

pub struct Langs {
    langs: Vec<Box<dyn Lang>>,
}

impl Langs {
    pub fn new(langs: Vec<Box<dyn Lang>>) -> Self {
        Self { langs }
    }

    pub fn for_path(&self, p: &RelPath) -> Option<&dyn Lang> {
        let ext = p.extension()?;
        self.langs
            .iter()
            .find(|l| l.extensions().iter().any(|e| *e == ext))
            .map(|b| &**b)
    }

    pub fn iter(&self) -> impl Iterator<Item = &dyn Lang> {
        self.langs.iter().map(|b| &**b)
    }
}

#[derive(Clone, Debug)]
pub struct RawEntity {
    pub kind: Kind,
    pub name: String,
    pub name_range: Option<ByteRange>,
    pub item_range: ByteRange,
    pub bytes_range: ByteRange,
    pub parent_idx: Option<usize>,
    pub children: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Resolution {
    pub slots: Vec<(ByteRange, crate::ids::Slot, Namespace)>,
    pub refs: Vec<(ByteRange, crate::content::IdentRef)>,
}
