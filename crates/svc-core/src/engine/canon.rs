use std::collections::HashMap;

use super::extract::byte_range;
use crate::content::{Content, IdentRef, Namespace, Token};
use crate::error::Result;
use crate::ids::{ByteRange, EntityId, Slot};
use crate::lang::{Env, Lang, Locator, Resolution, Role, When};

pub fn canonicalize(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    res: &Resolution,
    children: &[(ByteRange, EntityId)],
    lang: &dyn Lang,
) -> Result<Content> {
    let mut tokens = Vec::new();
    walk(
        item,
        item,
        src,
        lang,
        children,
        res,
        &binders_from_res(res),
        &mut tokens,
    );
    Ok(Content { tokens })
}

fn binders_from_res(res: &Resolution) -> HashMap<(u32, u32), (Slot, Namespace)> {
    let mut m = HashMap::new();
    for (r, slot, ns) in &res.slots {
        m.insert((r.start, r.end), (*slot, *ns));
    }
    m
}

pub fn resolve_locals(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    env: &Env,
) -> Resolution {
    let mut slots = Vec::new();
    let mut binders = Vec::new();
    let mut next: HashMap<Namespace, u32> = HashMap::new();
    inherit_outer_generics(item, src, lang, &mut slots, &mut binders, &mut next);
    collect_binders(
        item,
        src,
        lang,
        None,
        &mut slots,
        &mut binders,
        &mut next,
        item.id(),
    );
    let slot_at: HashMap<(u32, u32), (Slot, Namespace)> = slots
        .iter()
        .map(|(r, s, ns)| ((r.start, r.end), (*s, *ns)))
        .collect();
    let mut refs = Vec::new();
    collect_refs(
        item,
        src,
        lang,
        None,
        &slot_at,
        &binders,
        env,
        &mut refs,
        item.id(),
    );
    Resolution { slots, refs }
}

#[derive(Clone)]
struct BinderInfo {
    range: ByteRange,
    visible_from: u32,
    scope: ByteRange,
    slot: Slot,
    namespace: Namespace,
    name: String,
}

fn collect_binders<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    field: Option<&str>,
    slots: &mut Vec<(ByteRange, Slot, Namespace)>,
    binders: &mut Vec<BinderInfo>,
    next: &mut HashMap<Namespace, u32>,
    root_id: usize,
) {
    if skip_nested_item(node, lang, root_id) {
        return;
    }
    if is_opaque_node(node, lang) && node.id() != root_id {
        return;
    }
    for role in lang.roles(node, field, src, &crate::lang::Env::default()) {
        if let Role::Binder {
            namespace,
            visibility,
            locator,
        } = role
        {
            for name_node in locate(node, locator) {
                for id in ident_leaves(name_node, name_node) {
                    let r = byte_range(id);
                    // Typed `|x: T|` is both a `parameter` binder and a child of
                    // `closure_parameters`. One range, one slot.
                    if slots.iter().any(|(existing, _, _)| *existing == r) {
                        continue;
                    }
                    let n = next.entry(namespace).or_insert(0);
                    let slot = Slot(*n);
                    *n += 1;
                    slots.push((r, slot, namespace));
                    let (visible_from, scope) = binder_extent(node, src, lang, root_id, visibility);
                    binders.push(BinderInfo {
                        range: r,
                        visible_from,
                        scope,
                        slot,
                        namespace,
                        name: String::from_utf8_lossy(&src[r.start as usize..r.end as usize])
                            .into_owned(),
                    });
                }
            }
        }
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            collect_binders(
                c.node(),
                src,
                lang,
                c.field_name(),
                slots,
                binders,
                next,
                root_id,
            );
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn collect_refs<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    field: Option<&str>,
    slot_at: &HashMap<(u32, u32), (Slot, Namespace)>,
    binders: &[BinderInfo],
    env: &Env,
    refs: &mut Vec<(ByteRange, IdentRef)>,
    root_id: usize,
) {
    if skip_nested_item(node, lang, root_id) {
        return;
    }
    if is_opaque_node(node, lang) && node.id() != root_id {
        return;
    }
    if is_ident_leaf(node) {
        let r = byte_range(node);
        if slot_at.contains_key(&(r.start, r.end)) {
            // declaration site recorded as slot, not a ref
        } else {
            let name = String::from_utf8_lossy(&src[r.start as usize..r.end as usize]).into_owned();
            let ns = lang
                .roles(node, field, src, &crate::lang::Env::default())
                .into_iter()
                .find_map(|role| match role {
                    Role::Reference { namespace } => Some(namespace),
                    _ => None,
                })
                .unwrap_or(match node.kind() {
                    "type_identifier" | "primitive_type" => Namespace::Type,
                    "lifetime" => Namespace::Lifetime,
                    _ => Namespace::Value,
                });
            let local = binders
                .iter()
                .filter(|b| {
                    b.name == name
                        && b.namespace == ns
                        && b.visible_from <= r.start
                        && b.scope.start <= r.start
                        && r.end <= b.scope.end
                        && !blocked_by_barrier(node, b, ns, src, lang, root_id)
                })
                .max_by_key(|b| (b.scope.start, b.range.start));
            if let Some(binder) =
                local.filter(|_| !is_rust_nonlocal_ident(node, lang) && !is_struct_field_key(node))
            {
                refs.push((r, IdentRef::Local(binder.slot, ns)));
            } else if let Some(id) = env.lookup(&name, ns) {
                refs.push((r, IdentRef::Entity(id)));
            } else {
                refs.push((r, IdentRef::Free(name.into())));
            }
        }
        return;
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            collect_refs(
                c.node(),
                src,
                lang,
                c.field_name(),
                slot_at,
                binders,
                env,
                refs,
                root_id,
            );
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn locate<'a>(node: tree_sitter::Node<'a>, loc: Locator) -> Vec<tree_sitter::Node<'a>> {
    match loc {
        Locator::Itself => vec![node],
        Locator::Field(f) => node.child_by_field_name(f).into_iter().collect(),
        Locator::ChildIndex(i) => node.named_child(i as u32).into_iter().collect(),
        Locator::FieldWithKind(f, k) => node
            .child_by_field_name(f)
            .filter(|n| n.kind() == k)
            .into_iter()
            .collect(),
    }
}

fn ident_leaves<'a>(
    node: tree_sitter::Node<'a>,
    pattern_root: tree_sitter::Node<'a>,
) -> Vec<tree_sitter::Node<'a>> {
    if is_ident_leaf(node) {
        let t = node.kind();
        if t == "_" || node_is_wildcard(node) {
            return vec![];
        }
        return (!is_pattern_constructor(node, pattern_root))
            .then_some(node)
            .into_iter()
            .collect();
    }
    let mut out = Vec::new();
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        out.extend(ident_leaves(ch, pattern_root));
    }
    out
}

fn binder_extent(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    root_id: usize,
    visibility: crate::lang::Visibility,
) -> (u32, ByteRange) {
    match visibility {
        crate::lang::Visibility::AfterStmt => {
            let scope = enclosing_scope(node, src, lang, root_id);
            (node.end_byte() as u32, scope)
        }
        crate::lang::Visibility::Hoisted => {
            let scope = enclosing_var_scope(node, src, lang, root_id);
            (scope.start, scope)
        }
        crate::lang::Visibility::Sub(fields) => {
            let scope = sub_field_scope(node, fields)
                .unwrap_or_else(|| enclosing_scope(node, src, lang, root_id));
            (scope.start, scope)
        }
        crate::lang::Visibility::Whole
        | crate::lang::Visibility::Chain
        | crate::lang::Visibility::Inherit => {
            let scope = enclosing_scope(node, src, lang, root_id);
            (scope.start, scope)
        }
    }
}

fn enclosing_var_scope(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    root_id: usize,
) -> ByteRange {
    if lang.name() != "javascript" {
        return enclosing_scope(node, src, lang, root_id);
    }
    let mut current = node.parent();
    while let Some(parent) = current {
        if is_js_var_scope(parent.kind()) {
            return byte_range(parent);
        }
        if parent.id() == root_id {
            break;
        }
        current = parent.parent();
    }
    enclosing_scope(node, src, lang, root_id)
}

fn is_js_var_scope(kind: &str) -> bool {
    matches!(
        kind,
        "program"
            | "function_declaration"
            | "generator_function_declaration"
            | "function_expression"
            | "generator_function"
            | "method_definition"
            | "arrow_function"
            | "class_static_block"
    )
}

fn sub_field_scope(node: tree_sitter::Node<'_>, fields: &[&str]) -> Option<ByteRange> {
    let mut current = Some(node);
    while let Some(n) = current {
        let mut start = u32::MAX;
        let mut end = 0u32;
        let mut found = false;
        for field in fields {
            if let Some(child) = n.child_by_field_name(field) {
                let r = byte_range(child);
                start = start.min(r.start);
                end = end.max(r.end);
                found = true;
            }
        }
        if found {
            return Some(ByteRange { start, end });
        }
        current = n.parent();
    }
    None
}

fn enclosing_scope(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    root_id: usize,
) -> ByteRange {
    let mut current = node.parent();
    while let Some(parent) = current {
        if lang
            .roles(parent, None, src, &crate::lang::Env::default())
            .iter()
            .any(|role| matches!(role, Role::Scope { .. }))
        {
            return byte_range(parent);
        }
        if parent.id() == root_id {
            break;
        }
        current = parent.parent();
    }
    let mut root = node;
    while root.id() != root_id {
        root = root.parent().expect("resolver node belongs to root");
    }
    byte_range(root)
}

fn blocked_by_barrier(
    from: tree_sitter::Node<'_>,
    binder: &BinderInfo,
    ns: Namespace,
    src: &[u8],
    lang: &dyn Lang,
    root_id: usize,
) -> bool {
    let mut current = from.parent();
    while let Some(parent) = current {
        for role in lang.roles(parent, None, src, &crate::lang::Env::default()) {
            if let Role::Scope { barriers, .. } = role {
                let blocks = barriers
                    .iter()
                    .any(|b| b.ns == ns && b.when == When::Always);
                if blocks {
                    let scope = byte_range(parent);
                    let inside = scope.start <= binder.range.start && binder.range.end <= scope.end;
                    if !inside {
                        return true;
                    }
                }
            }
        }
        if parent.id() == root_id {
            break;
        }
        current = parent.parent();
    }
    false
}

/// Nested entities are skipped, so `When::ThroughBlock` cannot be applied by
/// walking into them. Copy ancestor `type_parameters` onto this item only when
/// the path does not go through a `block` (impl/trait methods inherit `T`;
/// a nested `fn` inside `fn f<T>` does not).
fn inherit_outer_generics(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    slots: &mut Vec<(ByteRange, Slot, Namespace)>,
    binders: &mut Vec<BinderInfo>,
    next: &mut HashMap<Namespace, u32>,
) {
    let scope = byte_range(item);
    let mut ancestor = item.parent();
    while let Some(node) = ancestor {
        if let Some(tp) = node.child_by_field_name("type_parameters") {
            if !crosses_block(node, item) {
                add_generic_binders(tp, src, lang, scope, slots, binders, next);
            }
        }
        ancestor = node.parent();
    }
}

fn crosses_block(ancestor: tree_sitter::Node<'_>, item: tree_sitter::Node<'_>) -> bool {
    let mut current = item.parent();
    while let Some(node) = current {
        if node.id() == ancestor.id() {
            return false;
        }
        if node.kind() == "block" {
            return true;
        }
        current = node.parent();
    }
    false
}

fn add_generic_binders(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    scope: ByteRange,
    slots: &mut Vec<(ByteRange, Slot, Namespace)>,
    binders: &mut Vec<BinderInfo>,
    next: &mut HashMap<Namespace, u32>,
) {
    if matches!(
        node.kind(),
        "type_parameter" | "lifetime_parameter" | "const_parameter"
    ) {
        for role in lang.roles(node, None, src, &Env::default()) {
            if let Role::Binder {
                namespace, locator, ..
            } = role
            {
                for name_node in locate(node, locator) {
                    for id in ident_leaves(name_node, name_node) {
                        let r = byte_range(id);
                        let n = next.entry(namespace).or_insert(0);
                        let slot = Slot(*n);
                        *n += 1;
                        slots.push((r, slot, namespace));
                        binders.push(BinderInfo {
                            range: r,
                            visible_from: scope.start,
                            scope,
                            slot,
                            namespace,
                            name: String::from_utf8_lossy(&src[r.start as usize..r.end as usize])
                                .into_owned(),
                        });
                    }
                }
            }
        }
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            add_generic_binders(c.node(), src, lang, scope, slots, binders, next);
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn is_pattern_constructor(
    node: tree_sitter::Node<'_>,
    pattern_root: tree_sitter::Node<'_>,
) -> bool {
    let mut current = node;
    while current.id() != pattern_root.id() {
        let Some(parent) = current.parent() else {
            break;
        };
        if matches!(parent.kind(), "tuple_struct_pattern" | "struct_pattern")
            && parent.child_by_field_name("type").is_some_and(|n| {
                n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte()
            })
        {
            return true;
        }
        current = parent;
    }
    false
}

fn is_struct_field_key(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "field_initializer" {
        return false;
    }
    parent.child_by_field_name("field").is_some_and(|f| {
        f.id() == node.id()
            || (f.start_byte() <= node.start_byte() && node.end_byte() <= f.end_byte())
    })
}

fn is_rust_nonlocal_ident(node: tree_sitter::Node<'_>, lang: &dyn Lang) -> bool {
    if lang.name() != "rust" {
        return false;
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(
            parent.kind(),
            "scoped_identifier" | "scoped_type_identifier"
        ) {
            return true;
        }
        if matches!(
            parent.kind(),
            "block" | "function_item" | "function_signature_item"
        ) {
            break;
        }
        current = parent;
    }
    false
}

fn node_is_wildcard(node: tree_sitter::Node<'_>) -> bool {
    matches!(node.kind(), "_" | "remaining_field_pattern")
}

/// Nested `function`/`class`/`impl` items are separate entities. JS
/// `variable_declarator` is an entity kind at module scope only; inside a
/// function it is a local and must not be a resolver barrier.
fn skip_nested_item(node: tree_sitter::Node<'_>, lang: &dyn Lang, root_id: usize) -> bool {
    if node.id() == root_id {
        return false;
    }
    if node.kind() == "variable_declarator" {
        return false;
    }
    lang.entity_kinds()
        .iter()
        .any(|r| r.node_kind == node.kind())
}

fn is_opaque_node(node: tree_sitter::Node<'_>, lang: &dyn Lang) -> bool {
    lang.opaque_nodes().iter().any(|k| *k == node.kind())
}

fn is_ident_leaf(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier"
            | "type_identifier"
            | "primitive_type"
            | "self"
            | "super"
            | "crate"
            | "lifetime"
            | "label"
            | "statement_identifier"
            | "shorthand_field_identifier"
            | "shorthand_property_identifier"
            | "shorthand_property_identifier_pattern"
    )
}

fn walk(
    node: tree_sitter::Node<'_>,
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    children: &[(ByteRange, EntityId)],
    res: &Resolution,
    binders: &HashMap<(u32, u32), (Slot, Namespace)>,
    tokens: &mut Vec<Token>,
) {
    if node.id() != item.id() {
        if let Some(id) = child_id(node, children) {
            tokens.push(Token::Child(id));
            return;
        }
    }
    if lang.trivia_kinds().iter().any(|k| *k == node.kind()) {
        return;
    }
    if is_opaque_node(node, lang) && node.id() != item.id() {
        let r = byte_range(node);
        let text = String::from_utf8_lossy(&src[r.start as usize..r.end as usize]);
        if !text.trim().is_empty() {
            tokens.push(Token::Lit(text.as_ref().into()));
        }
        return;
    }
    if node.child_count() == 0 {
        if let Some(tok) = leaf_token(node, src, item, res, binders) {
            tokens.push(tok);
        }
        return;
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            walk(c.node(), item, src, lang, children, res, binders, tokens);
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn child_id(node: tree_sitter::Node<'_>, children: &[(ByteRange, EntityId)]) -> Option<EntityId> {
    let start = node.start_byte() as u32;
    let end = node.end_byte() as u32;
    children
        .iter()
        .find(|(r, _)| r.end == end && start >= r.start && start <= r.end)
        .map(|(_, id)| *id)
}

fn leaf_token(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    item: tree_sitter::Node<'_>,
    res: &Resolution,
    binders: &HashMap<(u32, u32), (Slot, Namespace)>,
) -> Option<Token> {
    let r = byte_range(node);
    let text = String::from_utf8_lossy(&src[r.start as usize..r.end as usize]);
    if text.trim().is_empty() {
        return None;
    }
    if let Some((slot, ns)) = binders.get(&(r.start, r.end)) {
        return Some(Token::Binder(*slot, *ns));
    }
    if let Some(name) = item.child_by_field_name("name") {
        if byte_range(name) == r {
            return Some(Token::Ident(IdentRef::Entity(EntityId::SELF)));
        }
    }
    for (rr, ident) in &res.refs {
        if *rr == r {
            return Some(Token::Ident(ident.clone()));
        }
    }
    if is_ident_leaf(node)
        || matches!(
            node.kind(),
            "field_identifier" | "property_identifier" | "property_identifier_pattern"
        )
    {
        return Some(Token::Ident(IdentRef::Free(text.as_ref().into())));
    }
    let kind = node.kind();
    if kind.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        Some(Token::Kw(text.as_ref().into()))
    } else if matches!(
        kind,
        "string_literal"
            | "raw_string_literal"
            | "char_literal"
            | "integer_literal"
            | "float_literal"
            | "boolean_literal"
            | "raw_string_literal_text"
    ) {
        Some(Token::Lit(text.as_ref().into()))
    } else {
        Some(Token::Punct(text.as_ref().into()))
    }
}
