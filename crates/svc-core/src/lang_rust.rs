use crate::entity::Kind;
use crate::lang::{
    Barrier, BinderClass, CommutativeRule, EntityKindRule, Env, Lang, Locator, Role, Visibility,
    When,
};
use crate::content::Namespace;

pub struct RustLang;

const ENTITY_KINDS: &[EntityKindRule] = &[
    EntityKindRule {
        node_kind: "function_item",
        kind: Kind::Fn,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "function_signature_item",
        kind: Kind::Fn,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "struct_item",
        kind: Kind::Struct,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "enum_item",
        kind: Kind::Enum,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "union_item",
        kind: Kind::Union,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "trait_item",
        kind: Kind::Trait,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "impl_item",
        kind: Kind::Impl,
        name_field: None,
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "const_item",
        kind: Kind::Const,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "static_item",
        kind: Kind::Static,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "mod_item",
        kind: Kind::Mod,
        name_field: Some("name"),
        body_field: Some("body"),
        children_field: None,
    },
    EntityKindRule {
        node_kind: "type_item",
        kind: Kind::TypeAlias,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "macro_definition",
        kind: Kind::Macro,
        name_field: Some("name"),
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "use_declaration",
        kind: Kind::Opaque,
        name_field: None,
        body_field: None,
        children_field: None,
    },
    EntityKindRule {
        node_kind: "extern_crate_declaration",
        kind: Kind::Opaque,
        name_field: None,
        body_field: None,
        children_field: None,
    },
];

fn text(src: &[u8], node: tree_sitter::Node<'_>) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

impl Lang for RustLang {
    fn language(&self) -> tree_sitter::Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn name(&self) -> &'static str {
        "rust"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["rs"]
    }

    fn entity_kinds(&self) -> &'static [EntityKindRule] {
        ENTITY_KINDS
    }

    fn entity_name(&self, node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
        if node.kind() == "impl_item" {
            let ty = node.child_by_field_name("type").map(|n| text(src, n))?;
            return Some(match node.child_by_field_name("trait") {
                Some(tr) => format!("impl<{} for {ty}>", text(src, tr)),
                None => format!("impl<{ty}>"),
            });
        }
        // Opaque items are named by their text; a byte-offset fallback would give every
        // `use` below an edited line a new identity.
        if matches!(node.kind(), "use_declaration" | "extern_crate_declaration") {
            return Some(text(src, node));
        }
        ENTITY_KINDS
            .iter()
            .find(|r| r.node_kind == node.kind())
            .and_then(|r| r.name_field)
            .and_then(|f| node.child_by_field_name(f))
            .map(|n| text(src, n))
    }

    fn roles(
        &self,
        node: tree_sitter::Node<'_>,
        field: Option<&str>,
        _src: &[u8],
        _env: &Env,
    ) -> Vec<Role> {
        rust_roles(node, field)
    }

    // Macro arguments are not opaque: `vec![x]`, `matches!(k, ..)`, `format!("{x}")` use
    // the local, and renaming the binder without them does not compile (O9). Rust has
    // no node kind whose identifiers the compiler ignores.
    fn opaque_nodes(&self) -> &'static [&'static str] {
        &[]
    }

    fn commutative_parents(&self) -> &'static [CommutativeRule] {
        &[
            CommutativeRule {
                parent: None,
                only_child_kinds: None,
                except_child_kinds: Some(&[Kind::Macro]),
            },
            CommutativeRule {
                parent: Some(Kind::Mod),
                only_child_kinds: None,
                except_child_kinds: Some(&[Kind::Macro]),
            },
        ]
    }

    fn trivia_kinds(&self) -> &'static [&'static str] {
        &[
            "line_comment",
            "block_comment",
            "attribute_item",
            "inner_attribute_item",
        ]
    }
}

fn rust_roles(node: tree_sitter::Node<'_>, field: Option<&str>) -> Vec<Role> {
    match node.kind() {
        "function_item" | "function_signature_item" => vec![
            Role::Scope {
                opens: &[Namespace::Type, Namespace::Lifetime, Namespace::Value],
                barriers: &[
                    Barrier {
                        ns: Namespace::Value,
                        class: BinderClass::Local,
                        when: When::Always,
                    },
                    Barrier {
                        ns: Namespace::Value,
                        class: BinderClass::Generic,
                        when: When::ThroughBlock,
                    },
                    Barrier {
                        ns: Namespace::Type,
                        class: BinderClass::Generic,
                        when: When::ThroughBlock,
                    },
                    Barrier {
                        ns: Namespace::Lifetime,
                        class: BinderClass::Generic,
                        when: When::ThroughBlock,
                    },
                    Barrier {
                        ns: Namespace::Label,
                        class: BinderClass::Label,
                        when: When::Always,
                    },
                ],
            },
        ],
        "let_declaration" => vec![Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::AfterStmt,
            locator: Locator::Field("pattern"),
        }],
        "parameter" => vec![Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Sub(&["body"]),
            locator: Locator::Field("pattern"),
        }],
        "identifier" if field == Some("name") => vec![],
        // Untyped closure parameter `|s|`: a bare identifier under closure_parameters, scoped
        // to the closure. Typed ones are `parameter` nodes and bind through their pattern; a
        // locator over all of `parameters` would also bind the type names (O9 caught that).
        "identifier" if node.parent().is_some_and(|p| p.kind() == "closure_parameters") => {
            vec![Role::Binder {
                namespace: Namespace::Value,
                visibility: Visibility::Whole,
                locator: Locator::Itself,
            }]
        }
        "identifier" => vec![Role::Reference {
            namespace: Namespace::Value,
        }],
        "type_identifier" => vec![Role::Reference {
            namespace: Namespace::Type,
        }],
        "lifetime" => vec![Role::Reference {
            namespace: Namespace::Lifetime,
        }],
        "label" => match node.parent().map(|p| p.kind()) {
            Some("break_expression") | Some("continue_expression") => {
                vec![Role::Reference {
                    namespace: Namespace::Label,
                }]
            }
            _ => vec![Role::Binder {
                namespace: Namespace::Label,
                visibility: Visibility::Whole,
                locator: Locator::Itself,
            }],
        },
        "block" => vec![Role::Scope {
            opens: &[Namespace::Type, Namespace::Value, Namespace::Macro, Namespace::Label],
            barriers: &[],
        }],
        "closure_expression" => vec![
            Role::Scope {
                opens: &[Namespace::Value],
                barriers: &[Barrier {
                    ns: Namespace::Label,
                    class: BinderClass::Label,
                    when: When::Always,
                }],
            },
        ],
        "type_parameter" | "lifetime_parameter" | "const_parameter" => {
            let ns = match node.kind() {
                "lifetime_parameter" => Namespace::Lifetime,
                "const_parameter" => Namespace::Value,
                _ => Namespace::Type,
            };
            vec![Role::Binder {
                namespace: ns,
                visibility: Visibility::Whole,
                locator: Locator::Field("name"),
            }]
        }
        "self_parameter" => vec![Role::Binder {
            namespace: Namespace::Value,
            visibility: Visibility::Sub(&["body"]),
            locator: Locator::Itself,
        }],
        _ => vec![],
    }
}
