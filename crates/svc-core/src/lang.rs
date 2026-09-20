use std::collections::HashMap;
use std::sync::Arc;

use crate::content::Namespace;
use crate::entity::Kind;
use crate::ids::{ByteRange, EntityId, RelPath};

#[derive(Clone, Debug, Default)]
pub struct Env {
    pub names: Arc<HashMap<(String, Namespace), EntityId>>,
    /// File-root and nested defs keyed by the file they live in. A same-file
    /// `fn hex32` beats a same-named item in another crate when resolving.
    pub by_file: Arc<HashMap<RelPath, HashMap<(String, Namespace), EntityId>>>,
    /// Same-named items in `crates/<pkg>/**` share this map. File wins, then crate, then repo.
    pub by_crate: Arc<HashMap<String, HashMap<(String, Namespace), EntityId>>>,
    /// Inherent methods of the impl/class this item is being resolved in.
    /// `self.foo()` / `Self::foo()` / `this.foo()` look here, not in `names`
    /// (a free `fn foo` is a different target).
    pub self_methods: HashMap<String, EntityId>,
    /// When set, [`Self::lookup`] prefers [`Self::by_file`] for this path.
    pub current_file: Option<RelPath>,
}

impl Env {
    pub fn lookup(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        if let Some(file) = &self.current_file {
            if let Some(id) = Self::lookup_in_map(self.by_file.get(file), name, ns) {
                return Some(id);
            }
            if let Some(krate) = crate_key(file) {
                if let Some(id) = Self::lookup_in_map(self.by_crate.get(krate), name, ns) {
                    return Some(id);
                }
            }
        }
        self.lookup_global(name, ns)
    }

    pub fn lookup_global(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        Self::lookup_in_map(Some(self.names.as_ref()), name, ns)
    }

    fn lookup_in_map(
        map: Option<&HashMap<(String, Namespace), EntityId>>,
        name: &str,
        ns: Namespace,
    ) -> Option<EntityId> {
        let map = map?;
        map.get(&(name.to_string(), ns))
            .copied()
            .or_else(|| map.get(&(name.to_string(), Namespace::Any)).copied())
    }

    pub fn insert(&mut self, name: impl Into<String>, ns: Namespace, id: EntityId) {
        Arc::make_mut(&mut self.names).insert((name.into(), ns), id);
    }

    pub fn insert_def(&mut self, name: impl Into<String>, kind: Kind, id: EntityId) {
        self.insert_def_in(name, kind, id, None);
    }

    pub fn insert_def_in(
        &mut self,
        name: impl Into<String>,
        kind: Kind,
        id: EntityId,
        file: Option<&RelPath>,
    ) {
        let name = name.into();
        for ns in namespaces_for(kind) {
            if *ns == Namespace::Value
                && !is_value_primary(kind)
                && self.lookup_global(&name, Namespace::Value).is_some()
            {
                continue;
            }
            self.insert(&name, *ns, id);
            if let Some(file) = file {
                Arc::make_mut(&mut self.by_file)
                    .entry(file.clone())
                    .or_default()
                    .insert((name.clone(), *ns), id);
                if let Some(krate) = crate_key(file) {
                    Arc::make_mut(&mut self.by_crate)
                        .entry(krate.to_string())
                        .or_default()
                        .insert((name.clone(), *ns), id);
                }
            }
        }
        if is_value_primary(kind) {
            self.insert(&name, Namespace::Value, id);
            if let Some(file) = file {
                Arc::make_mut(&mut self.by_file)
                    .entry(file.clone())
                    .or_default()
                    .insert((name.clone(), Namespace::Value), id);
                if let Some(krate) = crate_key(file) {
                    Arc::make_mut(&mut self.by_crate)
                        .entry(krate.to_string())
                        .or_default()
                        .insert((name.clone(), Namespace::Value), id);
                }
            }
        }
    }

    pub fn insert_defs(&mut self, defs: &[(&str, Kind, EntityId)]) {
        for (name, kind, id) in defs {
            self.insert_def(*name, *kind, *id);
        }
    }
}

pub fn namespaces_for(kind: Kind) -> &'static [Namespace] {
    match kind {
        Kind::Fn | Kind::Const | Kind::Static | Kind::Macro => &[Namespace::Value],
        Kind::Struct | Kind::Enum => &[Namespace::Type, Namespace::Value],
        Kind::Union | Kind::Trait | Kind::TypeAlias => &[Namespace::Type],
        Kind::Mod => &[Namespace::Value, Namespace::Type],
        Kind::Impl | Kind::Opaque | Kind::JsStaticBlock => &[],
        Kind::JsFunction
        | Kind::JsClass
        | Kind::JsMethod
        | Kind::JsGetter
        | Kind::JsSetter
        | Kind::JsField
        | Kind::JsStaticMethod
        | Kind::JsStaticField
        | Kind::JsDeclarator => &[Namespace::Value],
    }
}

fn is_value_primary(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::Fn
            | Kind::Const
            | Kind::Static
            | Kind::Macro
            | Kind::JsFunction
            | Kind::JsClass
            | Kind::JsMethod
            | Kind::JsGetter
            | Kind::JsSetter
            | Kind::JsField
            | Kind::JsStaticMethod
            | Kind::JsStaticField
            | Kind::JsDeclarator
    )
}

/// `crates/svc-core/src/ids.rs` → `svc-core`. Paths outside `crates/` have no crate key.
fn crate_key(file: &RelPath) -> Option<&str> {
    file.as_str()
        .strip_prefix("crates/")
        .and_then(|rest| rest.split('/').next())
        .filter(|s| !s.is_empty())
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
    Hoisted,
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
    /// Text to wrap a lone member in so it parses, when a definition is added to or edited
    /// under a parent of `parent` kind: a JS method is not a program on its own, a Rust
    /// impl method is an item. `None` means the member parses standalone.
    fn member_shell(&self, _parent: Kind) -> Option<(&'static str, &'static str)> {
        None
    }

    /// Leaf node kinds whose text is a literal (numbers, strings, chars, booleans);
    /// canonicalisation keeps their text as `Token::Lit`.
    fn literal_kinds(&self) -> &'static [&'static str] {
        &[]
    }
    /// Override the table kind for nodes that share a grammar kind (JS getters/setters).
    fn refine_kind(&self, _node: tree_sitter::Node<'_>, _src: &[u8]) -> Option<Kind> {
        None
    }
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
