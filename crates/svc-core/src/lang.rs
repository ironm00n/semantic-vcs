use std::collections::{HashMap, HashSet};
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
    /// Same-named items in `crates/<pkg>/**` share this map. File wins, then a
    /// name unique in the crate, then a name unique in the repo; collisions are Free.
    pub by_crate: Arc<HashMap<String, HashMap<(String, Namespace), EntityId>>>,
    /// Crate-level names that appear in more than one file. A third file must not
    /// bind last-wins (DOGFOOD: two file-level items with one name in one crate).
    pub crate_ambiguous: Arc<HashMap<String, HashSet<(String, Namespace)>>>,
    /// Repo-wide names that appear under more than one definition.
    pub names_ambiguous: Arc<HashSet<(String, Namespace)>>,
    /// Occupants that own this (name, ns) as their primary namespace (`fn` in
    /// Value, not `struct` which also sits in Value as a constructor).
    pub primary: Arc<HashSet<(String, Namespace)>>,
    /// Per-crate primary occupants; same rule as [`Self::primary`].
    pub crate_primary: Arc<HashMap<String, HashSet<(String, Namespace)>>>,
    /// Inherent methods and associated consts of the impl/class this item is
    /// being resolved in. `self.foo()` / `Self::foo` / `Self::N` / `this.foo()`
    /// look here, not in `names` (a free `fn foo` is a different target).
    pub self_methods: HashMap<String, EntityId>,
    /// Items nested in the function (or JS function/method) being resolved —
    /// `fn f() { fn g() {} g(); }`. They are not in [`Self::by_file`]: a sibling
    /// `fn h() { g(); }` must not bind to `f`'s helper, and a nested `fn parse`
    /// must not make a file-level `parse` in the same crate look ambiguous.
    pub nested_items: HashMap<(String, Namespace), EntityId>,
    /// When set, [`Self::lookup`] prefers [`Self::by_file`] for this path.
    pub current_file: Option<RelPath>,
    /// Inside `mod inner { … }`, bare names are the module's own items only.
    /// `crate::` still uses [`Self::lookup_module`].
    pub in_nested_mod: bool,
    /// Parent-mod maps for `super::` / `super::super::` / … Index 0 is one
    /// `super` (the parent of the enclosing mod). Past the last nested mod,
    /// [`Self::lookup_super`] falls through to [`Self::lookup_module`].
    pub super_stack: Vec<HashMap<(String, Namespace), EntityId>>,
    /// Children of each `mod` entity, for `crate::outer::parse` / `super::inner::f`.
    pub mod_items: HashMap<EntityId, HashMap<(String, Namespace), EntityId>>,
    /// Enclosing `mod` entity, for `self::inner::f`.
    pub self_mod: Option<EntityId>,
    /// File loaded by `mod foo;` → that `mod` entity. Items in the file are
    /// inside the module, not the crate root.
    pub file_of_mod: HashMap<RelPath, EntityId>,
    /// Declaring file of a `mod foo;` entity (`src/lib.rs` for `mod outer;`).
    pub mod_decl_file: HashMap<EntityId, RelPath>,
    /// Parent files for `super::` / `super::super::` out of a file module.
    pub super_files: Vec<RelPath>,
    /// Inside an inline `mod inner { }` (not only a file loaded by `mod foo;`).
    pub inline_mod: bool,
    /// Names brought in by `use` in this module. File-module isolation drops
    /// unique-crate lookup, so `use super::parse; parse()` must still bind.
    pub use_imports: HashMap<(String, Namespace), EntityId>,
    /// `use path::h as hh` — `hh` is a local spelling, not an Entity hole.
    pub use_aliases: HashSet<String>,
    /// `pub use` names visible as `crate::parse` / `crate::engine::foo`.
    pub file_reexports: HashMap<RelPath, HashMap<(String, Namespace), EntityId>>,
    /// `pub use` inside a `mod`, for `crate::engine::foo` after `pub use merge::foo`.
    pub mod_reexports: HashMap<EntityId, HashMap<(String, Namespace), EntityId>>,
    /// When set, [`crate::engine::canon`] records pub uses into the reexport maps.
    pub bind_reexports: bool,
    /// Every `mod` entity, so `use a as b; b::parse` is a module path, not Type::name.
    pub mods: HashSet<EntityId>,
}

impl Env {
    pub fn lookup(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        if let Some(id) = Self::lookup_in_map(Some(&self.nested_items), name, ns) {
            return Some(id);
        }
        if let Some(m) = self.self_mod {
            if let Some(id) = Self::lookup_in_map(self.mod_items.get(&m), name, ns) {
                return Some(id);
            }
        }
        if let Some(id) = Self::lookup_in_map(Some(&self.use_imports), name, ns) {
            return Some(id);
        }
        if self.use_aliases.contains(name) {
            return None;
        }
        if self.in_nested_mod {
            return None;
        }
        self.lookup_module(name, ns)
    }

    /// `super::f` (`depth` 1) / `super::super::f` (`depth` 2). Past the last
    /// nested parent module, this is the crate root.
    pub fn lookup_super(&self, name: &str, ns: Namespace, depth: usize) -> Option<EntityId> {
        if depth == 0 {
            return None;
        }
        let i = depth - 1;
        if let Some(map) = self.super_stack.get(i) {
            return Self::lookup_in_map(Some(map), name, ns);
        }
        let skip = usize::from(self.inline_mod);
        if i < skip {
            return self.lookup_module(name, ns);
        }
        if let Some(file) = self.super_files.get(i - skip) {
            return Self::lookup_in_map(self.by_file.get(file), name, ns);
        }
        self.lookup_module(name, ns)
    }

    /// `crate::a::b` — `a` is crate-root, then each segment is a child of that mod.
    pub fn lookup_crate_path(&self, segs: &[String], ns: Namespace) -> Option<EntityId> {
        if segs.is_empty() {
            return None;
        }
        if segs.len() == 1 {
            return self.lookup_crate_root(&segs[0], ns);
        }
        let start = self.lookup_crate_root(&segs[0], Namespace::Type)?;
        self.walk_mod_path(start, &segs[1..], ns)
    }

    /// `crate::parse` is the crate root, not a same-named item in this file module.
    fn lookup_crate_root(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        if let Some(file) = self.super_files.last() {
            return Self::lookup_in_map(self.by_file.get(file), name, ns)
                .or_else(|| Self::lookup_in_map(self.file_reexports.get(file), name, ns));
        }
        if let Some(file) = &self.current_file {
            if let Some(id) = Self::lookup_in_map(self.by_file.get(file), name, ns).or_else(|| {
                Self::lookup_in_map(self.file_reexports.get(file), name, ns)
            }) {
                return Some(id);
            }
        }
        self.lookup_module(name, ns)
    }

    /// `super::a::b` after `depth` `super::` prefixes.
    pub fn lookup_super_path(
        &self,
        depth: usize,
        segs: &[String],
        ns: Namespace,
    ) -> Option<EntityId> {
        if segs.is_empty() {
            return None;
        }
        if segs.len() == 1 {
            return self.lookup_super(&segs[0], ns, depth);
        }
        let start = self.lookup_super(&segs[0], Namespace::Type, depth)?;
        self.walk_mod_path(start, &segs[1..], ns)
    }

    /// `self::a::b` in the enclosing module.
    pub fn lookup_self_path(&self, segs: &[String], ns: Namespace) -> Option<EntityId> {
        if segs.is_empty() {
            return None;
        }
        let start_mod = self.self_mod?;
        if segs.len() == 1 {
            return self.lookup_in_mod(start_mod, &segs[0], ns);
        }
        let start = self.lookup_in_mod(start_mod, &segs[0], Namespace::Type)?;
        self.walk_mod_path(start, &segs[1..], ns)
    }

    fn lookup_in_mod(&self, m: EntityId, name: &str, ns: Namespace) -> Option<EntityId> {
        Self::lookup_in_map(self.mod_items.get(&m), name, ns)
            .or_else(|| Self::lookup_in_map(self.mod_reexports.get(&m), name, ns))
    }

    fn walk_mod_path(&self, start: EntityId, rest: &[String], ns: Namespace) -> Option<EntityId> {
        if rest.is_empty() {
            return Some(start);
        }
        let mut id = start;
        for s in &rest[..rest.len() - 1] {
            id = self.lookup_in_mod(id, s, Namespace::Type)?;
        }
        self.lookup_in_mod(id, rest.last()?, ns)
    }

    /// `use a as b; b::parse` — `b` names a module, then walk its children.
    pub fn lookup_aliased_mod_path(&self, segs: &[String], ns: Namespace) -> Option<EntityId> {
        if segs.len() < 2 {
            return None;
        }
        let start = self.lookup(&segs[0], Namespace::Type)?;
        if !self.mods.contains(&start) {
            return None;
        }
        self.walk_mod_path(start, &segs[1..], ns)
    }

    pub fn is_mod(&self, id: EntityId) -> bool {
        self.mods.contains(&id)
    }

    /// File / crate / repo names, skipping nested-mod isolation. `crate::f`
    /// inside `mod inner` still binds the crate-root item.
    pub fn lookup_module(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        if let Some(file) = &self.current_file {
            if let Some(id) = Self::lookup_in_map(self.by_file.get(file), name, ns) {
                return Some(id);
            }
            if let Some(krate) = crate_key(file) {
                if !self
                    .crate_ambiguous
                    .get(krate)
                    .is_some_and(|s| Self::is_ambiguous(s, name, ns))
                {
                    if let Some(id) = Self::lookup_in_map(self.by_crate.get(krate), name, ns) {
                        return Some(id);
                    }
                }
            }
        }
        self.lookup_global(name, ns)
    }

    pub fn lookup_global(&self, name: &str, ns: Namespace) -> Option<EntityId> {
        if Self::is_ambiguous(&self.names_ambiguous, name, ns) {
            return None;
        }
        Self::lookup_in_map(Some(self.names.as_ref()), name, ns)
    }

    fn is_ambiguous(set: &HashSet<(String, Namespace)>, name: &str, ns: Namespace) -> bool {
        set.contains(&(name.to_string(), ns))
            || set.contains(&(name.to_string(), Namespace::Any))
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
        self.insert_ranked(name, ns, id, false);
    }

    fn insert_ranked(
        &mut self,
        name: impl Into<String>,
        ns: Namespace,
        id: EntityId,
        primary: bool,
    ) {
        let name = name.into();
        Self::occupy(
            Arc::make_mut(&mut self.names),
            Arc::make_mut(&mut self.names_ambiguous),
            Arc::make_mut(&mut self.primary),
            &name,
            ns,
            id,
            primary,
        );
    }

    /// Two primaries (`fn parse` in two files) or two secondaries are
    /// ambiguous. A primary may replace a secondary (`fn Foo` after
    /// `struct Foo`) without last-wins Free — they share a spelling, not a
    /// namespace.
    fn occupy(
        map: &mut HashMap<(String, Namespace), EntityId>,
        amb: &mut HashSet<(String, Namespace)>,
        primaries: &mut HashSet<(String, Namespace)>,
        name: &str,
        ns: Namespace,
        id: EntityId,
        primary: bool,
    ) {
        let key = (name.to_string(), ns);
        if let Some(old) = map.get(&key) {
            if *old != id {
                let old_primary = primaries.contains(&key);
                if old_primary && !primary {
                    return;
                }
                if old_primary == primary {
                    amb.insert(key.clone());
                }
            }
        }
        map.insert(key.clone(), id);
        if primary {
            primaries.insert(key);
        }
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
        if kind == Kind::Mod {
            self.mods.insert(id);
        }
        for ns in namespaces_for(kind) {
            if *ns == Namespace::Value
                && !is_value_primary(kind)
                && self.lookup_global(&name, Namespace::Value).is_some()
            {
                continue;
            }
            let primary = is_value_primary(kind) && *ns == Namespace::Value;
            self.insert_ranked(&name, *ns, id, primary);
            if let Some(file) = file {
                Arc::make_mut(&mut self.by_file)
                    .entry(file.clone())
                    .or_default()
                    .insert((name.clone(), *ns), id);
                if let Some(krate) = crate_key(file) {
                    self.insert_crate(krate, &name, *ns, id, primary);
                }
            }
        }
        if is_value_primary(kind) {
            self.insert_ranked(&name, Namespace::Value, id, true);
            if let Some(file) = file {
                Arc::make_mut(&mut self.by_file)
                    .entry(file.clone())
                    .or_default()
                    .insert((name.clone(), Namespace::Value), id);
                if let Some(krate) = crate_key(file) {
                    self.insert_crate(krate, &name, Namespace::Value, id, true);
                }
            }
        }
    }

    fn insert_crate(
        &mut self,
        krate: &str,
        name: &str,
        ns: Namespace,
        id: EntityId,
        primary: bool,
    ) {
        Self::occupy(
            Arc::make_mut(&mut self.by_crate)
                .entry(krate.to_string())
                .or_default(),
            Arc::make_mut(&mut self.crate_ambiguous)
                .entry(krate.to_string())
                .or_default(),
            Arc::make_mut(&mut self.crate_primary)
                .entry(krate.to_string())
                .or_default(),
            name,
            ns,
            id,
            primary,
        );
    }

    pub fn insert_defs(&mut self, defs: &[(&str, Kind, EntityId)]) {
        for (name, kind, id) in defs {
            self.insert_def(*name, *kind, *id);
        }
    }

    /// Bind a nested item for the duration of one `resolve`. Last insert wins
    /// so an inner function's helper shadows one on an enclosing function.
    pub fn insert_nested(&mut self, name: impl Into<String>, kind: Kind, id: EntityId) {
        let name = name.into();
        for ns in namespaces_for(kind) {
            self.nested_items.insert((name.clone(), *ns), id);
        }
    }

    pub fn insert_mod_child(
        &mut self,
        parent: EntityId,
        name: impl Into<String>,
        kind: Kind,
        id: EntityId,
    ) {
        self.mods.insert(parent);
        if kind == Kind::Mod {
            self.mods.insert(id);
        }
        let map = self.mod_items.entry(parent).or_default();
        Self::insert_super_level(map, name, kind, id);
    }

    pub fn insert_super_level(
        map: &mut HashMap<(String, Namespace), EntityId>,
        name: impl Into<String>,
        kind: Kind,
        id: EntityId,
    ) {
        let name = name.into();
        for ns in namespaces_for(kind) {
            map.insert((name.clone(), *ns), id);
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
    /// `#[path = "bar.rs"]` on `mod foo;` — the file that is this module.
    pub path_attr: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Resolution {
    pub slots: Vec<(ByteRange, crate::ids::Slot, Namespace)>,
    pub refs: Vec<(ByteRange, crate::content::IdentRef)>,
}
