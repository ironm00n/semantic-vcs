//! JavaScript language module over tree-sitter-javascript 0.25.
//!
//! Binding data transcribed from the JS binding tables (scope §1,
//! binder §2, reference §3); trap IDs below (T1–T23) refer to its §4.
//! Contradiction resolutions applied: C1 (accessor/static live in `Kind`,
//! design decision §25), C5 (`Namespace::Label` exists).

use crate::content::Namespace;
use crate::entity::Kind;
use crate::lang::{
    Barrier, BinderClass, CommutativeRule, EntityKindRule, Env, Lang, Locator, Role, Visibility,
    When,
};

pub struct JsLang;

const ENTITY_KINDS: &[EntityKindRule] = &[
    EntityKindRule {
        node_kind: "function_declaration",
        kind: Kind::JsFunction,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "generator_function_declaration",
        kind: Kind::JsFunction,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "class_declaration",
        kind: Kind::JsClass,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    // Table kind is the base; `js_kind` refines method/field/declarator.
    // (design note §8: flat table cannot hold 1 node kind → 4 Kinds.)
    EntityKindRule {
        node_kind: "method_definition",
        kind: Kind::JsMethod,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "field_definition",
        kind: Kind::JsField,
        name_field: Some("property"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "class_static_block",
        kind: Kind::JsStaticBlock,
        name_field: None,
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "variable_declarator",
        kind: Kind::JsDeclarator,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "import_statement",
        kind: Kind::Opaque,
        name_field: None,
        body_field: None,
        children_field: None,
    },
];

fn text(src: &[u8], node: tree_sitter::Node<'_>) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

/// Anonymous (token) children texts, e.g. `static`/`get`/`set` on members (T8).
fn anon_flags(node: tree_sitter::Node<'_>, src: &[u8]) -> (bool, bool, bool) {
    let mut cursor = node.walk();
    let mut is_static = false;
    let mut is_get = false;
    let mut is_set = false;
    for child in node.children(&mut cursor) {
        if child.is_named() {
            continue;
        }
        match &src[child.start_byte()..child.end_byte()] {
            b"static" => is_static = true,
            b"get" => is_get = true,
            b"set" => is_set = true,
            _ => {}
        }
    }
    (is_static, is_get, is_set)
}

/// Refined entity kind for nodes whose `Kind` needs more than the node kind:
/// accessor/static members (C1/T8), function-valued declarators, static fields.
/// Returns `None` for nodes the flat table already classifies.
pub fn js_kind(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<Kind> {
    match node.kind() {
        "method_definition" => {
            let (is_static, is_get, is_set) = anon_flags(node, src);
            // Accessor wins over static: a static get/set pair keeps working;
            // only static+instance same-name-same-accessor collides (rare).
            Some(if is_get {
                Kind::JsGetter
            } else if is_set {
                Kind::JsSetter
            } else if is_static {
                Kind::JsStaticMethod
            } else {
                Kind::JsMethod
            })
        }
        "field_definition" => Some(if anon_flags(node, src).0 {
            Kind::JsStaticField
        } else {
            Kind::JsField
        }),
        "variable_declarator" => {
            let is_fn = node
                .child_by_field_name("value")
                .is_some_and(|v| matches!(v.kind(), "function_expression" | "generator_function" | "arrow_function"));
            Some(if is_fn {
                Kind::JsFunction
            } else {
                Kind::JsDeclarator
            })
        }
        _ => None,
    }
}

fn parent_node(node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    node.parent()
}

/// `function_declaration` is var-scoped at the top of a function body or
/// `program`, block-scoped in nested blocks (T10). Unwraps `export_statement`.
fn is_top_level_fn(node: tree_sitter::Node<'_>) -> bool {
    let mut cur = node;
    if parent_node(cur).is_some_and(|p| p.kind() == "export_statement") {
        cur = cur.parent().unwrap();
    }
    match parent_node(cur) {
        Some(p) if p.kind() == "program" => true,
        Some(p) if p.kind() == "statement_block" => is_function_body_block(&p),
        _ => false,
    }
}

const FN_KINDS: [&str; 7] = [
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "generator_function",
    "arrow_function",
    "method_definition",
    "class_static_block",
];

fn is_function_body_block(block: &tree_sitter::Node<'_>) -> bool {
    block.kind() == "statement_block"
        && block.parent().is_some_and(|p| {
            FN_KINDS.contains(&p.kind()) && p.child_by_field_name("body").is_some_and(|b| b.id() == block.id())
        })
}

/// `for_in_statement` `kind:` texts via the anonymous-token field (T1).
fn for_in_kinds(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    let mut cursor = node.walk();
    node.children_by_field_name("kind", &mut cursor)
        .map(|k| String::from_utf8_lossy(&src[k.start_byte()..k.end_byte()]).into_owned())
        .collect()
}

fn has_export_source(spec: tree_sitter::Node<'_>) -> bool {
    // specifier → export_clause → export_statement; `source:` present means
    // re-export from another module, so `name:` is not a local ref (T4).
    spec.parent()
        .and_then(|c| c.parent())
        .and_then(|c| c.parent())
        .and_then(|s| s.child_by_field_name("source"))
        .is_some()
}

fn binder(ns: Namespace, vis: Visibility, loc: Locator) -> Vec<Role> {
    vec![Role::Binder {
        namespace: ns,
        visibility: vis,
        locator: loc,
    }]
}

fn scope_label_barrier() -> Role {
    Role::Scope {
        opens: &[Namespace::Value],
        barriers: &[Barrier {
            ns: Namespace::Label,
            class: BinderClass::Label,
            when: When::Always,
        }],
    }
}

fn js_roles(node: tree_sitter::Node<'_>, field: Option<&str>, src: &[u8]) -> Vec<Role> {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => vec![
            scope_label_barrier(),
            Role::Binder {
                namespace: Namespace::Value,
                visibility: if is_top_level_fn(node) {
                    Visibility::Hoisted
                } else {
                    Visibility::Whole
                },
                locator: Locator::Field("name"),
            },
        ],
        // Named function expression self-binding: visible only inside (facts §2).
        "function_expression" | "generator_function" | "class" => binder(
            Namespace::Value,
            Visibility::Whole,
            Locator::Field("name"),
        ),
        "class_declaration" => vec![
            Role::Scope {
                opens: &[Namespace::Value],
                barriers: &[],
            },
            Role::Binder {
                namespace: Namespace::Value,
                visibility: Visibility::Whole,
                locator: Locator::Field("name"),
            },
        ],
        "variable_declarator" => {
            // `variable_declaration` is `var` (node kind is the discriminator, T1-adjacent).
            let is_var = parent_node(node).is_some_and(|p| p.kind() == "variable_declaration");
            let vis = if is_var { Visibility::Hoisted } else { Visibility::Whole };
            binder(Namespace::Value, vis, Locator::Field("name"))
        }
        "formal_parameters" => binder(Namespace::Value, Visibility::Whole, Locator::Itself),
        "catch_clause" => {
            // No `parameter:` means no catch scope, only the body block (facts §1).
            if node.child_by_field_name("parameter").is_none() {
                return vec![];
            }
            vec![
            Role::Scope {
                opens: &[Namespace::Value],
                barriers: &[],
            },
            Role::Binder {
                namespace: Namespace::Value,
                visibility: Visibility::Whole,
                locator: Locator::Field("parameter"),
            }
        ]
        }
        // Binder iff a `kind:` child exists (T3); var hoists, let/const/using loop-scope.
        "for_in_statement" => {
            let kinds = for_in_kinds(node, src);
            if kinds.is_empty() {
                vec![Role::Scope {
                    opens: &[Namespace::Value],
                    barriers: &[],
                }]
            } else {
                let vis = if kinds.iter().any(|k| k == "var") {
                    Visibility::Hoisted
                } else {
                    Visibility::Whole
                };
                vec![
                    Role::Scope {
                        opens: &[Namespace::Value],
                        barriers: &[],
                    },
                    Role::Binder {
                        namespace: Namespace::Value,
                        visibility: vis,
                        locator: Locator::Field("left"),
                    },
                ]
            }
        }
        "program"
        | "class_body"
        | "class_static_block"
        | "switch_body"
        | "method_definition" => vec![Role::Scope {
            opens: &[Namespace::Value],
            barriers: &[],
        }],
        "statement_block" => {
            if is_function_body_block(&node) {
                vec![]
            } else {
                vec![Role::Scope {
                    opens: &[Namespace::Value, Namespace::Label],
                    barriers: &[],
                }]
            }
        }
        "arrow_function" => vec![scope_label_barrier()],
        // `for (let …;;)` loop scope; `var`/expression forms bind nothing new.
        "for_statement" => {
            let lexical = node
                .child_by_field_name("initializer")
                .is_some_and(|i| i.kind() == "lexical_declaration");
            if lexical {
                vec![Role::Scope {
                    opens: &[Namespace::Value],
                    barriers: &[],
                }]
            } else {
                vec![]
            }
        }
        "identifier" => identifier_roles(node, field),
        // Object-literal `{x}` is a value reference (facts §3, T6).
        "shorthand_property_identifier" => vec![Role::Reference {
            namespace: Namespace::Value,
        }],
        "statement_identifier" => {
            if parent_node(node).is_some_and(|p| p.kind() == "labeled_statement") {
                binder(Namespace::Label, Visibility::Whole, Locator::Itself)
            } else {
                vec![Role::Reference {
                    namespace: Namespace::Label,
                }]
            }
        }
        _ => vec![],
    }
}

/// Identifier classification: binders (import bindings, arrow single param),
/// export references, declaration-site silence, else value reference.
fn identifier_roles(node: tree_sitter::Node<'_>, field: Option<&str>) -> Vec<Role> {
    let Some(parent) = parent_node(node) else {
        return vec![Role::Reference {
            namespace: Namespace::Value,
        }];
    };
    match (Some(parent.kind()), field) {
        // Single-unparenthesized-param arrow: no `formal_parameters` exists (T12).
        (Some("arrow_function"), Some("parameter")) => {
            binder(Namespace::Value, Visibility::Whole, Locator::Itself)
        }
        (Some("import_clause"), None) => {
            // Default import: direct `identifier` child (verified in probe).
            binder(Namespace::Value, Visibility::Whole, Locator::Itself)
        }
        (Some("namespace_import"), _) => {
            binder(Namespace::Value, Visibility::Whole, Locator::Itself)
        }
        (Some("import_specifier"), Some("alias")) => {
            binder(Namespace::Value, Visibility::Whole, Locator::Itself)
        }
        (Some("import_specifier"), Some("name")) => {
            // Binder is `alias ?? name`; `name` with an alias present is the
            // other module's export-table name, never local (T4).
            if node
                .parent()
                .and_then(|p| p.child_by_field_name("alias"))
                .is_none()
            {
                binder(Namespace::Value, Visibility::Whole, Locator::Itself)
            } else {
                vec![]
            }
        }
        (Some("export_specifier"), Some("name")) => {
            if has_export_source(node) {
                vec![]
            } else {
                vec![Role::Reference {
                    namespace: Namespace::Value,
                }]
            }
        }
        (Some("export_specifier"), _) => vec![],
        // Declaration-site names are covered by the parent's Binder role.
        (_, Some("name")) => vec![],
        _ => vec![Role::Reference {
            namespace: Namespace::Value,
        }],
    }
}

impl Lang for JsLang {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_javascript::LANGUAGE.into()
    }

    fn name(&self) -> &'static str {
        "javascript"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["js", "mjs", "cjs"]
    }

    fn entity_kinds(&self) -> &'static [EntityKindRule] {
        ENTITY_KINDS
    }

    fn refine_kind(&self, node: tree_sitter::Node<'_>, src: &[u8]) -> Option<Kind> {
        js_kind(node, src)
    }

    fn entity_name(&self, node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
        match node.kind() {
            // Static blocks have no declared name; use the block's own text
            // (Rust `use` lines got the same fix in txvzzzpm) so the id
            // survives edits above it instead of encoding a byte offset.
            "class_static_block" => Some(text(src, node)),
            // Opaque import entity keys on the module specifier so sibling
            // imports usually differ in `(parent, kind, name)`.
            "import_statement" => node
                .child_by_field_name("source")
                .map(|s| text(src, s)),
            _ => ENTITY_KINDS
                .iter()
                .find(|r| r.node_kind == node.kind())
                .and_then(|r| r.name_field)
                .and_then(|f| node.child_by_field_name(f))
                .map(|n| text(src, n)),
        }
    }

    fn roles(
        &self,
        node: tree_sitter::Node<'_>,
        field: Option<&str>,
        src: &[u8],
        _env: &Env,
    ) -> Vec<Role> {
        js_roles(node, field, src)
    }

    fn opaque_nodes(&self) -> &'static [&'static str] {
        &[]
    }

    fn commutative_parents(&self) -> &'static [CommutativeRule] {
        // `class_body` is commutative for methods only: field initializers
        // run in order, so field order is semantic (design §9, facts C6).
        &[CommutativeRule {
            parent: Some(Kind::JsClass),
            only_child_kinds: Some(&[
                Kind::JsMethod,
                Kind::JsGetter,
                Kind::JsSetter,
                Kind::JsStaticMethod,
            ]),
            except_child_kinds: None,
        }]
    }

    fn trivia_kinds(&self) -> &'static [&'static str] {
        &["comment"]
    }

    fn member_shell(&self, parent: Kind) -> Option<(&'static str, &'static str)> {
        matches!(parent, Kind::JsClass).then_some(("class __svc_shell__ {", "\n}\n"))
    }
}
