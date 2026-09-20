use std::collections::HashMap;

use super::extract::{byte_range, is_extracted_item};
use crate::content::{Content, IdentRef, Namespace, Token};
use crate::error::Result;
use crate::ids::{ByteRange, EntityId, RelPath, Slot};
use crate::lang::{BinderClass, Env, Lang, Locator, Resolution, Role, When};

pub fn canonicalize(
    item: tree_sitter::Node<'_>,
    src: &[u8],
    res: &Resolution,
    children: &[(ByteRange, EntityId)],
    lang: &dyn Lang,
) -> Result<Content> {
    let mut tokens = Vec::new();
    let binders = binders_from_res(res);
    let attached = file_attached_nodes(item, lang);
    let item_start = item.start_byte();
    for extra in &attached {
        if extra.start_byte() < item_start {
            walk(
                *extra,
                item,
                src,
                lang,
                children,
                res,
                &binders,
                &mut tokens,
            );
        }
    }
    walk(item, item, src, lang, children, res, &binders, &mut tokens);
    for extra in &attached {
        if extra.start_byte() >= item_start {
            walk(
                *extra,
                item,
                src,
                lang,
                children,
                res,
                &binders,
                &mut tokens,
            );
        }
    }
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
    // File-tail / inter-item statements are not entities. Resolve them as
    // part of the neighbouring file-root so `rename` rewrites uses there.
    // Walk extras against their own binders: the file-root's name must stay
    // an Entity hole (Local would remain a literal in this item's bytes).
    let mut extra_binders = Vec::new();
    let extra_binder_start = slots.len();
    for extra in file_attached_nodes(item, lang) {
        collect_binders(
            extra,
            src,
            lang,
            None,
            &mut slots,
            &mut extra_binders,
            &mut next,
            extra.id(),
        );
    }
    let extra_slot_at: HashMap<(u32, u32), (Slot, Namespace)> = slots[extra_binder_start..]
        .iter()
        .map(|(r, s, ns)| ((r.start, r.end), (*s, *ns)))
        .collect();
    for extra in file_attached_nodes(item, lang) {
        collect_refs(
            extra,
            src,
            lang,
            None,
            &extra_slot_at,
            &extra_binders,
            env,
            &mut refs,
            extra.id(),
        );
    }
    Resolution { slots, refs }
}

/// Named file-level statements owned by this file-root but not part of its
/// item node: preceding kindless statements (in this entity's bytes, which
/// start where the previous file-root's item ended) and, when this is the
/// last file-root, following tail statements through EOF.
fn file_attached_nodes<'a>(
    item: tree_sitter::Node<'a>,
    lang: &dyn Lang,
) -> Vec<tree_sitter::Node<'a>> {
    if !is_extracted_item(item, lang) || !is_file_root_item(item, lang) {
        return Vec::new();
    }
    let root = syntax_root(item);
    let stmt = top_level_under(item, root);
    let mut children = Vec::new();
    let mut c = root.walk();
    for ch in root.named_children(&mut c) {
        children.push(ch);
    }
    let Some(idx) = children.iter().position(|ch| ch.id() == stmt.id()) else {
        return Vec::new();
    };
    let prev_entity = (0..idx)
        .rev()
        .find(|&i| subtree_has_extracted_entity(children[i], lang));
    let next_entity =
        ((idx + 1)..children.len()).find(|&i| subtree_has_extracted_entity(children[i], lang));
    let start = prev_entity.map(|i| i + 1).unwrap_or(0);
    let last = next_entity.is_none();
    let mut out = Vec::new();
    for (i, ch) in children.into_iter().enumerate() {
        if i == idx {
            continue;
        }
        if i < start {
            continue;
        }
        if i < idx {
            out.push(ch);
            continue;
        }
        if last {
            out.push(ch);
        }
    }
    out
}

fn is_file_root_item(item: tree_sitter::Node<'_>, lang: &dyn Lang) -> bool {
    let mut p = item.parent();
    while let Some(parent) = p {
        if is_extracted_item(parent, lang) {
            return false;
        }
        p = parent.parent();
    }
    true
}

fn syntax_root(item: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    let mut n = item;
    while let Some(p) = n.parent() {
        n = p;
    }
    n
}

fn top_level_under<'a>(
    item: tree_sitter::Node<'a>,
    root: tree_sitter::Node<'a>,
) -> tree_sitter::Node<'a> {
    let mut n = item;
    while let Some(p) = n.parent() {
        if p.id() == root.id() {
            return n;
        }
        n = p;
    }
    item
}

fn subtree_has_extracted_entity(node: tree_sitter::Node<'_>, lang: &dyn Lang) -> bool {
    if is_extracted_item(node, lang) {
        return true;
    }
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if subtree_has_extracted_entity(ch, lang) {
            return true;
        }
    }
    false
}

#[derive(Clone)]
struct BinderInfo {
    range: ByteRange,
    visible_from: u32,
    scope: ByteRange,
    slot: Slot,
    namespace: Namespace,
    class: BinderClass,
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
                for id in ident_leaves(name_node, name_node, src) {
                    let r = byte_range(id);
                    // Typed `|x: T|` is both a `parameter` binder and a child of
                    // `closure_parameters`. One range, one slot.
                    if slots.iter().any(|(existing, _, _)| *existing == r) {
                        continue;
                    }
                    let name = String::from_utf8_lossy(&src[r.start as usize..r.end as usize])
                        .into_owned();
                    // `Ok(x) | Err(x)` is one binding. Fresh slots per occurrence
                    // make O9 rename them apart and rustc reject the or-pattern.
                    let slot =
                        or_pattern_slot(id, &name, namespace, binders).unwrap_or_else(|| {
                            let n = next.entry(namespace).or_insert(0);
                            let slot = Slot(*n);
                            *n += 1;
                            slot
                        });
                    slots.push((r, slot, namespace));
                    let (visible_from, scope) = binder_extent(node, src, lang, root_id, visibility);
                    binders.push(BinderInfo {
                        range: r,
                        visible_from,
                        scope,
                        slot,
                        namespace,
                        class: binder_class(node),
                        name,
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
    if is_inherent_method_ref(node, src) {
        let r = byte_range(node);
        let name = String::from_utf8_lossy(&src[r.start as usize..r.end as usize]).into_owned();
        if let Some(id) = env.self_methods.get(&name) {
            refs.push((r, IdentRef::Entity(*id)));
        } else {
            refs.push((r, IdentRef::Free(name.into())));
        }
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
            if is_struct_field_key(node)
                || is_dot_field(node)
                || is_type_binding_name(node)
                || is_use_alias(node)
                || in_attribute(node)
            {
                refs.push((r, IdentRef::Free(name.into())));
            } else if let Some(binder) = local
                .filter(|_| !is_rust_nonlocal_ident(node, lang))
                .filter(|b| !const_context_hides_binder(node, b))
            {
                refs.push((r, IdentRef::Local(binder.slot, ns)));
            } else if is_foreign_scoped_ref(node, src, lang, env)
                || is_type_qualified_ref(node, src, lang, env)
            {
                refs.push((r, IdentRef::Free(name.into())));
            } else if env.alias_spellings.contains(&name) {
                refs.push((r, IdentRef::Free(name.into())));
            } else if let Some(id) = if let Some(segs) = crate_path_segs(node, src, lang) {
                env.lookup_crate_path(&segs, ns).or_else(|| {
                    if under_use_tree(node) {
                        env.lookup_module(&name, ns)
                    } else {
                        None
                    }
                })
            } else if let Some((depth, segs)) = super_path_parts(node, src, lang) {
                env.lookup_super_path(depth, &segs, ns).or_else(|| {
                    if under_use_tree(node) {
                        env.lookup_module(&name, ns)
                    } else {
                        None
                    }
                })
            } else if let Some(segs) = self_path_segs(node, src, lang) {
                env.lookup_self_path(&segs, ns).or_else(|| {
                    if under_use_tree(node) {
                        env.lookup_module(&name, ns)
                    } else {
                        None
                    }
                })
            } else if let Some(segs) = aliased_mod_segs(node, src, lang) {
                env.lookup_aliased_mod_path(&segs, ns)
                    .or_else(|| env.lookup(&name, ns))
            } else {
                env.lookup(&name, ns)
            } {
                refs.push((r, IdentRef::Entity(id)));
            } else if let Some(ident) = const_generic_arg_ref(
                node,
                &name,
                r,
                binders,
                env,
                src,
                lang,
                root_id,
            ) {
                refs.push((r, ident));
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
    src: &[u8],
) -> Vec<tree_sitter::Node<'a>> {
    if is_ident_leaf(node) {
        let t = node.kind();
        if t == "_" || node_is_wildcard(node) {
            return vec![];
        }
        return (!is_pattern_constructor(node, pattern_root, src))
            .then_some(node)
            .into_iter()
            .collect();
    }
    if node.kind() == "match_pattern" {
        let mut out = Vec::new();
        let mut c = node.walk();
        if c.goto_first_child() {
            loop {
                if c.field_name() != Some("condition") {
                    out.extend(ident_leaves(c.node(), pattern_root, src));
                }
                if !c.goto_next_sibling() {
                    break;
                }
            }
        }
        return out;
    }
    let mut out = Vec::new();
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        out.extend(ident_leaves(ch, pattern_root, src));
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
            let visible_from = node.end_byte() as u32;
            if node.kind() == "let_condition" {
                if let Some(parent) = node.parent() {
                    let end = match parent.kind() {
                        "if_expression" => parent
                            .child_by_field_name("consequence")
                            .map(|c| c.end_byte() as u32),
                        "while_expression" => {
                            parent.child_by_field_name("body").map(|c| c.end_byte() as u32)
                        }
                        _ => None,
                    };
                    if let Some(end) = end {
                        return (
                            visible_from,
                            ByteRange {
                                start: visible_from,
                                end,
                            },
                        );
                    }
                }
            }
            (visible_from, enclosing_scope(node, src, lang, root_id))
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
        crate::lang::Visibility::Whole => {
            if node.kind() == "label" {
                if let Some(parent) = node.parent() {
                    if parent.kind() == "for_expression" {
                        if let Some(body) = parent.child_by_field_name("body") {
                            let scope = byte_range(body);
                            return (scope.start, scope);
                        }
                    }
                }
            }
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
                let blocks = barriers.iter().any(|b| {
                    b.ns == ns && b.when == When::Always && b.class == binder.class
                });
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
                    for id in ident_leaves(name_node, name_node, src) {
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
                            class: BinderClass::Generic,
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
    src: &[u8],
) -> bool {
    let mut current = node;
    while current.id() != pattern_root.id() {
        let Some(parent) = current.parent() else {
            break;
        };
        match parent.kind() {
            "tuple_struct_pattern" | "struct_pattern" => {
                if parent.child_by_field_name("type").is_some_and(|n| {
                    n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte()
                }) {
                    return true;
                }
            }
            "scoped_identifier" | "scoped_type_identifier" | "generic_pattern" => {
                return true;
            }
            "field_pattern" => {
                if node.kind() != "shorthand_field_identifier"
                    && parent.child_by_field_name("name").is_some_and(|n| {
                        n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte()
                    })
                {
                    return true;
                }
            }
            "captured_pattern" => {
                if parent.named_child(0).is_some_and(|n| n.id() == node.id()) {
                    return false;
                }
            }
            _ => {}
        }
        current = parent;
    }
    // Unit variants / consts (`None`, `AfterStmt`) are identifier patterns.
    // Bindings in this crate are snake_case; PascalCase is the constructor.
    // A const-generic `N` is an identifier but not a pattern.
    if node.kind() == "identifier"
        && !node
            .parent()
            .is_some_and(|p| p.kind() == "const_parameter")
    {
        let name = &src[node.start_byte()..node.end_byte()];
        if name.first().is_some_and(|b| b.is_ascii_uppercase()) {
            return true;
        }
    }
    false
}

fn or_pattern_slot(
    ident: tree_sitter::Node<'_>,
    name: &str,
    ns: Namespace,
    binders: &[BinderInfo],
) -> Option<Slot> {
    let mut current = ident.parent();
    while let Some(parent) = current {
        if parent.kind() == "or_pattern" {
            let start = parent.start_byte() as u32;
            let end = parent.end_byte() as u32;
            if let Some(b) = binders.iter().rev().find(|b| {
                b.namespace == ns && b.name == name && start <= b.range.start && b.range.end <= end
            }) {
                return Some(b.slot);
            }
        }
        current = parent.parent();
    }
    None
}

fn is_struct_field_key(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    match parent.kind() {
        "field_initializer" => parent.child_by_field_name("field").is_some_and(|f| {
            f.id() == node.id()
                || (f.start_byte() <= node.start_byte() && node.end_byte() <= f.end_byte())
        }),
        // `S { id: x }`: `id` names the field (SPEC §9, no namespace), not a
        // local or `fn id`. Shorthand `S { id }` is a binder, not a key.
        "field_pattern" => {
            node.kind() != "shorthand_field_identifier"
                && parent.child_by_field_name("name").is_some_and(|n| {
                    n.id() == node.id()
                        || (n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte())
                })
        }
        _ => false,
    }
}

/// `use crate::a::h as hh`: `hh` is the local alias, not the imported
/// entity's name. Rename of `h` must not rewrite the alias.
fn is_use_alias(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "use_as_clause" {
        return false;
    }
    parent.child_by_field_name("alias").is_some_and(|n| {
        n.id() == node.id()
            || (n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte())
    })
}

/// `#[parse]` is an attribute / derive / lint name, not the value-namespace
/// `fn parse`. A later proc-macro identity can bind Macro; until then Free.
fn in_attribute(mut node: tree_sitter::Node<'_>) -> bool {
    loop {
        let Some(parent) = node.parent() else {
            return false;
        };
        if matches!(
            parent.kind(),
            "attribute_item" | "inner_attribute_item" | "attribute"
        ) {
            return true;
        }
        node = parent;
    }
}

/// `It<Item = T>`: the left `Item` is an associated-type binding name, not a
/// reference to a type `Item` in scope (SPEC §9; needs the receiver's type).
fn is_type_binding_name(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    if parent.kind() != "type_binding" {
        return false;
    }
    parent.child_by_field_name("name").is_some_and(|n| {
        n.id() == node.id()
            || (n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte())
    })
}

/// `.src` is a field or method name, including inside `vec![self.src.len()]`
/// where the path is a flat `token_tree`. A same-named local must not win.
fn is_dot_field(node: tree_sitter::Node<'_>) -> bool {
    let mut prev = node.prev_sibling();
    while let Some(n) = prev {
        if n.kind() == "." {
            return true;
        }
        if n.is_named() {
            return false;
        }
        prev = n.prev_sibling();
    }
    false
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

/// `RedbStore::open` is not a free `fn open`. Type-relative names stay Free
/// (SPEC §9) unless they are same-impl inherent methods (earlier) or the
/// qualifier is `Self` / the enclosing impl or trait (`S::Item` / `T::Item` /
/// `Tr::Item` in `impl Tr for S` is the associated type, not a free `Item`).
fn is_type_qualified_ref(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    env: &Env,
) -> bool {
    if lang.name() != "rust" {
        return false;
    }
    if under_use_tree(node) {
        return false;
    }
    if let Some(root) = scoped_path_root(node) {
        let root_name =
            std::str::from_utf8(&src[root.start_byte()..root.end_byte()]).unwrap_or("");
        if matches!(root_name, "crate" | "super" | "self") {
            return false;
        }
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    if !matches!(
        parent.kind(),
        "scoped_identifier" | "scoped_type_identifier"
    ) {
        return false;
    }
    if parent.child_by_field_name("name").map(|n| n.id()) != Some(node.id()) {
        return false;
    }
    let Some(path) = parent.child_by_field_name("path") else {
        return false;
    };
    let Some(qual) = scoped_qualifier(path) else {
        return false;
    };
    let name = std::str::from_utf8(&src[qual.start_byte()..qual.end_byte()]).unwrap_or("");
    if matches!(name, "crate" | "super" | "self" | "Self") {
        return false;
    }
    if Some(name.as_bytes()) == enclosing_impl_type_name(node, src)
        || Some(name.as_bytes()) == enclosing_impl_trait_name(node, src)
    {
        return false;
    }
    if env
        .lookup(name, Namespace::Type)
        .is_some_and(|id| env.is_mod(id))
    {
        return false;
    }
    env.lookup_global(name, Namespace::Type).is_some()
}

fn scoped_qualifier(mut node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    loop {
        match node.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                node = node.child_by_field_name("name")?;
            }
            "generic_type" => {
                node = node.child_by_field_name("type")?;
            }
            "bracketed_type" => {
                node = node.named_child(0)?;
            }
            "qualified_type" => {
                node = node.child_by_field_name("type")?;
            }
            _ => return Some(node),
        }
    }
}

fn under_use_tree(mut node: tree_sitter::Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if matches!(
            parent.kind(),
            "use_declaration" | "use_as_clause" | "use_list" | "use_wildcard"
        ) {
            return true;
        }
        node = parent;
    }
    false
}

/// `axum::response` is not our `fn response` in another file. If the leftmost
/// path segment is not `crate`/`super`/`self` and is not an entity, every
/// later segment is Free. `use crate::a::f` must not look foreign: wrapping
/// `use` nodes are not the path root.
fn is_foreign_scoped_ref(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
    env: &Env,
) -> bool {
    if lang.name() != "rust" {
        return false;
    }
    let Some(root) = scoped_path_root(node) else {
        return false;
    };
    if root.id() == node.id() {
        return false;
    }
    let name = std::str::from_utf8(&src[root.start_byte()..root.end_byte()]).unwrap_or("");
    if matches!(name, "crate" | "super" | "self" | "Self") {
        return false;
    }
    if env.lookup(name, Namespace::Type).is_some() || env.lookup(name, Namespace::Value).is_some() {
        return false;
    }
    env.lookup_global(name, Namespace::Value).is_none()
        && env.lookup_global(name, Namespace::Type).is_none()
}

fn aliased_mod_segs(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
) -> Option<Vec<String>> {
    if lang.name() != "rust" {
        return None;
    }
    let segs = scoped_path_idents(node, src)?;
    if segs.len() < 2 {
        return None;
    }
    if matches!(segs[0].as_str(), "crate" | "super" | "self" | "Self") {
        return None;
    }
    Some(segs)
}

fn is_crate_path(node: tree_sitter::Node<'_>, src: &[u8], lang: &dyn Lang) -> bool {
    path_root_is(node, src, lang, "crate")
}

fn crate_path_segs(node: tree_sitter::Node<'_>, src: &[u8], lang: &dyn Lang) -> Option<Vec<String>> {
    if !is_crate_path(node, src, lang) {
        return None;
    }
    let mut segs = scoped_path_idents(node, src)?;
    if segs.first().map(String::as_str) != Some("crate") {
        return None;
    }
    segs.remove(0);
    Some(segs)
}

fn super_path_parts(
    node: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
) -> Option<(usize, Vec<String>)> {
    if lang.name() != "rust" {
        return None;
    }
    let segs = scoped_path_idents(node, src)?;
    let depth = segs.iter().take_while(|s| s.as_str() == "super").count();
    if depth == 0 {
        return None;
    }
    Some((depth, segs[depth..].to_vec()))
}

fn self_path_segs(node: tree_sitter::Node<'_>, src: &[u8], lang: &dyn Lang) -> Option<Vec<String>> {
    if lang.name() != "rust" {
        return None;
    }
    let mut segs = scoped_path_idents(node, src)?;
    if segs.first().map(String::as_str) != Some("self") {
        return None;
    }
    segs.remove(0);
    Some(segs)
}

fn scoped_path_idents(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<Vec<String>> {
    let parent = node.parent()?;
    if !matches!(parent.kind(), "scoped_identifier" | "scoped_type_identifier") {
        return None;
    }
    let path = parent.child_by_field_name("path")?;
    let mut segs = path_idents(path, src);
    segs.push(node_text(node, src));
    Some(segs)
}

fn path_idents(node: tree_sitter::Node<'_>, src: &[u8]) -> Vec<String> {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut segs = node
                .child_by_field_name("path")
                .map(|p| path_idents(p, src))
                .unwrap_or_default();
            if let Some(name) = node.child_by_field_name("name") {
                segs.push(node_text(name, src));
            }
            segs
        }
        "bracketed_type" => node
            .named_child(0)
            .map(|n| path_idents(n, src))
            .unwrap_or_default(),
        "qualified_type" => node
            .child_by_field_name("type")
            .map(|n| path_idents(n, src))
            .unwrap_or_default(),
        "generic_type" => node
            .child_by_field_name("type")
            .map(|n| path_idents(n, src))
            .unwrap_or_default(),
        _ => vec![node_text(node, src)],
    }
}

fn node_text(node: tree_sitter::Node<'_>, src: &[u8]) -> String {
    std::str::from_utf8(&src[node.start_byte()..node.end_byte()])
        .unwrap_or("")
        .to_string()
}

/// `use` names in the same module as `item`. Nested inline mods keep their
/// own imports; they do not inherit the outer file's.
pub(crate) fn fill_use_imports(
    env: &mut Env,
    item: tree_sitter::Node<'_>,
    src: &[u8],
    lang: &dyn Lang,
) {
    env.use_imports.clear();
    env.use_aliases.clear();
    if lang.name() != "rust" {
        return;
    }
    collect_use_imports(env, use_scope_node(item), src);
    collect_nested_use_imports(env, item, src);
}

fn collect_nested_use_imports(env: &mut Env, item: tree_sitter::Node<'_>, src: &[u8]) {
    walk_nested_uses(env, item, src, item.id());
}

fn walk_nested_uses(
    env: &mut Env,
    node: tree_sitter::Node<'_>,
    src: &[u8],
    item_id: usize,
) {
    if node.id() != item_id && is_nested_use_barrier(node.kind()) {
        return;
    }
    if node.kind() == "use_declaration" {
        bind_use_declaration(env, node, src);
        return;
    }
    let mut c = node.walk();
    if c.goto_first_child() {
        loop {
            walk_nested_uses(env, c.node(), src, item_id);
            if !c.goto_next_sibling() {
                break;
            }
        }
    }
}

fn is_nested_use_barrier(kind: &str) -> bool {
    matches!(
        kind,
        "function_item"
            | "function_signature_item"
            | "mod_item"
            | "impl_item"
            | "trait_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "const_item"
            | "static_item"
            | "type_item"
            | "macro_definition"
            | "extern_crate_declaration"
    )
}

fn use_scope_node(mut node: tree_sitter::Node<'_>) -> tree_sitter::Node<'_> {
    loop {
        let Some(parent) = node.parent() else {
            return node;
        };
        if node.kind() == "declaration_list" && parent.kind() == "mod_item" {
            return node;
        }
        if parent.kind() == "source_file" {
            return parent;
        }
        node = parent;
    }
}

pub(crate) fn collect_use_imports(env: &mut Env, root: tree_sitter::Node<'_>, src: &[u8]) {
    let mut c = root.walk();
    if !c.goto_first_child() {
        return;
    }
    loop {
        let n = c.node();
        if n.kind() == "use_declaration" {
            bind_use_declaration(env, n, src);
        }
        if !c.goto_next_sibling() {
            break;
        }
    }
}

fn bind_use_declaration(env: &mut Env, n: tree_sitter::Node<'_>, src: &[u8]) {
    if env.bind_reexports && !use_is_pub(n, src) {
        return;
    }
    if let Some(arg) = n.child_by_field_name("argument") {
        import_use_tree(env, arg, src, &[]);
        return;
    }
    for i in 0..n.named_child_count() {
        let Some(ch) = n.named_child(i as u32) else {
            continue;
        };
        if ch.kind() == "visibility_modifier" {
            continue;
        }
        import_use_tree(env, ch, src, &[]);
        break;
    }
}

fn use_is_pub(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    if let Some(v) = node.child_by_field_name("visibility") {
        return node_text(v, src).contains("pub");
    }
    for i in 0..node.named_child_count() {
        if let Some(ch) = node.named_child(i as u32) {
            if ch.kind() == "visibility_modifier" {
                return node_text(ch, src).contains("pub");
            }
        }
    }
    false
}

fn import_use_tree(env: &mut Env, node: tree_sitter::Node<'_>, src: &[u8], prefix: &[String]) {
    match node.kind() {
        "use_as_clause" => {
            let Some(path) = node.child_by_field_name("path") else {
                return;
            };
            let mut segs = prefix.to_vec();
            segs.extend(path_idents(path, src));
            let name = node
                .child_by_field_name("alias")
                .map(|a| node_text(a, src))
                .or_else(|| segs.last().cloned())
                .unwrap_or_default();
            bind_use(env, &segs, &name);
        }
        "use_list" => {
            for i in 0..node.named_child_count() {
                if let Some(ch) = node.named_child(i as u32) {
                    import_use_tree(env, ch, src, prefix);
                }
            }
        }
        "scoped_use_list" => {
            let mut segs = prefix.to_vec();
            if let Some(p) = node.child_by_field_name("path") {
                segs.extend(path_idents(p, src));
            }
            if let Some(list) = node.child_by_field_name("list") {
                import_use_tree(env, list, src, &segs);
                return;
            }
            for i in 0..node.named_child_count() {
                if let Some(ch) = node.named_child(i as u32) {
                    if ch.kind() == "use_list" {
                        import_use_tree(env, ch, src, &segs);
                    }
                }
            }
        }
        "use_wildcard" => {
            let mut segs = prefix.to_vec();
            for i in 0..node.named_child_count() {
                if let Some(ch) = node.named_child(i as u32) {
                    if ch.kind() == "*" || node_text(ch, src) == "*" {
                        continue;
                    }
                    segs.extend(path_idents(ch, src));
                }
            }
            bind_glob(env, &segs);
        }
        _ => {
            let mut segs = prefix.to_vec();
            segs.extend(path_idents(node, src));
            if segs.last().map(|s| s.as_str()) == Some("self") && segs.len() >= 2 {
                let alias = segs[segs.len() - 2].clone();
                bind_use(env, &segs[..segs.len() - 1], &alias);
                return;
            }
            let Some(name) = segs.last().cloned() else {
                return;
            };
            if name == "*" {
                bind_glob(env, &segs[..segs.len() - 1]);
                return;
            }
            bind_use(env, &segs, &name);
        }
    }
}

fn bind_glob(env: &mut Env, prefix: &[String]) {
    let Some(map) = glob_names(env, prefix) else {
        return;
    };
    for (k, id) in map {
        if env.bind_reexports {
            insert_reexport(env, k.0, k.1, id);
        } else {
            env.use_imports.insert(k, id);
        }
    }
}

fn insert_reexport(env: &mut Env, name: String, ns: Namespace, id: EntityId) {
    if let Some(file) = env.current_file.clone() {
        env.file_reexports
            .entry(file)
            .or_default()
            .insert((name.clone(), ns), id);
    }
    if let Some(m) = env.self_mod {
        env.mod_reexports
            .entry(m)
            .or_default()
            .insert((name, ns), id);
    }
}

fn glob_names(
    env: &Env,
    prefix: &[String],
) -> Option<HashMap<(String, Namespace), EntityId>> {
    if prefix.is_empty() {
        return None;
    }
    if prefix.len() == 1 && prefix[0] == "super" {
        return super_glob_map(env);
    }
    if prefix[0] == "crate" {
        if prefix.len() == 1 {
            return crate_root_glob(env);
        }
        let m = env.lookup_crate_path(&prefix[1..], Namespace::Type)?;
        return glob_mod(env, m);
    }
    if prefix[0] == "self" {
        if prefix.len() == 1 {
            return env
                .self_mod
                .and_then(|m| glob_mod(env, m))
                .or_else(|| {
                    env.current_file
                        .as_ref()
                        .and_then(|f| merge_file_glob(env, f))
                });
        }
        let m = env.lookup_self_path(&prefix[1..], Namespace::Type)?;
        return glob_mod(env, m);
    }
    if prefix[0] == "super" {
        let depth = prefix.iter().take_while(|s| s.as_str() == "super").count();
        let rest = &prefix[depth..];
        if rest.is_empty() {
            return super_glob_at(env, depth);
        }
        let m = env.lookup_super_path(depth, rest, Namespace::Type)?;
        return glob_mod(env, m);
    }
    let m = env
        .lookup_self_path(prefix, Namespace::Type)
        .or_else(|| env.lookup_crate_path(prefix, Namespace::Type))?;
    glob_mod(env, m)
}

fn glob_mod(
    env: &Env,
    m: EntityId,
) -> Option<HashMap<(String, Namespace), EntityId>> {
    let mut map = env.mod_items.get(&m).cloned().unwrap_or_default();
    if let Some(re) = env.mod_reexports.get(&m) {
        map.extend(re.clone());
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn crate_root_glob(env: &Env) -> Option<HashMap<(String, Namespace), EntityId>> {
    let file = env.super_files.last().or(env.current_file.as_ref())?;
    merge_file_glob(env, file)
}

fn merge_file_glob(
    env: &Env,
    file: &RelPath,
) -> Option<HashMap<(String, Namespace), EntityId>> {
    let mut map = env.by_file.get(file).cloned().unwrap_or_default();
    if let Some(re) = env.file_reexports.get(file) {
        map.extend(re.clone());
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn super_glob_map(env: &Env) -> Option<HashMap<(String, Namespace), EntityId>> {
    super_glob_at(env, 1)
}

fn super_glob_at(env: &Env, depth: usize) -> Option<HashMap<(String, Namespace), EntityId>> {
    if depth == 0 {
        return None;
    }
    let i = depth - 1;
    if let Some(map) = env.super_stack.get(i) {
        return Some(map.clone());
    }
    let skip = usize::from(env.inline_mod);
    if i < skip {
        let file = env.current_file.as_ref()?;
        return merge_file_glob(env, file);
    }
    let file = env.super_files.get(i - skip)?;
    merge_file_glob(env, file)
}

fn bind_use(env: &mut Env, segs: &[String], alias: &str) {
    if alias.is_empty() || segs.is_empty() {
        return;
    }
    let imported = segs
        .iter()
        .rev()
        .find(|s| !matches!(s.as_str(), "self" | "super" | "crate"))
        .map(|s| s.as_str())
        .unwrap_or("");
    let aliased = !imported.is_empty() && alias != imported;
    if aliased {
        env.alias_spellings.insert(alias.to_string());
    }
    for ns in [Namespace::Value, Namespace::Type] {
        let Some(id) = resolve_use_path(env, segs, ns) else {
            if env.bind_reexports {
                continue;
            }
            if aliased {
                env.use_aliases.insert(alias.to_string());
            }
            continue;
        };
        if env.bind_reexports {
            if !aliased || env.is_mod(id) {
                insert_reexport(env, alias.to_string(), ns, id);
            }
            continue;
        }
        if aliased && !env.is_mod(id) {
            env.use_aliases.insert(alias.to_string());
            continue;
        }
        env.use_imports.insert((alias.to_string(), ns), id);
    }
}

fn resolve_use_path(env: &Env, segs: &[String], ns: Namespace) -> Option<EntityId> {
    if segs.is_empty() {
        return None;
    }
    let segs = if segs.last().map(|s| s.as_str()) == Some("self") && segs.len() >= 2 {
        &segs[..segs.len() - 1]
    } else {
        segs
    };
    match segs[0].as_str() {
        "crate" => env.lookup_crate_path(&segs[1..], ns),
        "super" => {
            let depth = segs.iter().take_while(|s| s.as_str() == "super").count();
            env.lookup_super_path(depth, &segs[depth..], ns)
        }
        "self" => env.lookup_self_path(&segs[1..], ns),
        _ => env
            .lookup_self_path(segs, ns)
            .or_else(|| env.lookup_crate_path(segs, ns)),
    }
}

fn path_root_is(node: tree_sitter::Node<'_>, src: &[u8], lang: &dyn Lang, want: &str) -> bool {
    if lang.name() != "rust" {
        return false;
    }
    let Some(root) = scoped_path_root(node) else {
        return false;
    };
    if root.id() == node.id() {
        return false;
    }
    let name = std::str::from_utf8(&src[root.start_byte()..root.end_byte()]).unwrap_or("");
    name == want
}

fn scoped_path_root(mut node: tree_sitter::Node<'_>) -> Option<tree_sitter::Node<'_>> {
    let mut saw_scoped = false;
    while let Some(parent) = node.parent() {
        match parent.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                saw_scoped = true;
                node = parent;
            }
            // `use crate::a::f` / `use crate::a::g as gg`: stop at the wrapper so
            // the root stays `crate`, not the whole use line (which looks foreign).
            _ => break,
        }
    }
    if !saw_scoped {
        return None;
    }
    loop {
        match node.kind() {
            "scoped_identifier" | "scoped_type_identifier" => {
                node = node.child_by_field_name("path")?;
            }
            "bracketed_type" => {
                node = node.named_child(0)?;
            }
            "qualified_type" => {
                node = node.child_by_field_name("type")?;
            }
            "generic_type" => {
                node = node.child_by_field_name("type")?;
            }
            _ => return Some(node),
        }
    }
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

fn binder_class(node: tree_sitter::Node<'_>) -> BinderClass {
    match node.kind() {
        "type_parameter" | "lifetime_parameter" | "const_parameter" => BinderClass::Generic,
        "lifetime"
            if node
                .parent()
                .is_some_and(|p| p.kind() == "for_lifetimes") =>
        {
            BinderClass::Generic
        }
        "label" | "statement_identifier" => BinderClass::Label,
        _ => BinderClass::Local,
    }
}

/// Array lengths and const generic args cannot capture ordinary locals or
/// labels (SPEC; rustc E0435). Generic lifetimes are also out of a const
/// *value* (`{ 'a }` / array length) but still bind in a type arg (`Vec<&'a T>`).
fn const_context_hides_binder(node: tree_sitter::Node<'_>, binder: &BinderInfo) -> bool {
    match binder.class {
        BinderClass::Local | BinderClass::Label => in_const_value_position(node),
        BinderClass::Generic if binder.namespace == Namespace::Lifetime => {
            in_const_lifetime_position(node)
        }
        _ => false,
    }
}

/// `g::<n>()` parses `n` as a type_identifier. When that is not a type, it is
/// a const generic argument: a const-generic binder or a Value entity, never
/// an ordinary local (SPEC / rustc E0435).
fn const_generic_arg_ref(
    node: tree_sitter::Node<'_>,
    name: &str,
    range: ByteRange,
    binders: &[BinderInfo],
    env: &Env,
    src: &[u8],
    lang: &dyn Lang,
    root_id: usize,
) -> Option<IdentRef> {
    if node.kind() != "type_identifier" || !in_type_arguments(node) {
        return None;
    }
    let local = binders
        .iter()
        .filter(|b| {
            b.name == name
                && b.namespace == Namespace::Value
                && b.class == BinderClass::Generic
                && b.visible_from <= range.start
                && b.scope.start <= range.start
                && range.end <= b.scope.end
                && !blocked_by_barrier(node, b, Namespace::Value, src, lang, root_id)
        })
        .max_by_key(|b| (b.scope.start, b.range.start));
    if let Some(binder) = local {
        return Some(IdentRef::Local(binder.slot, Namespace::Value));
    }
    env.lookup(name, Namespace::Value).map(IdentRef::Entity)
}

fn in_array_length(node: tree_sitter::Node<'_>) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if matches!(parent.kind(), "array_type" | "array_expression") {
            if parent.child_by_field_name("length").is_some_and(|n| {
                n.start_byte() <= node.start_byte() && node.end_byte() <= n.end_byte()
            }) {
                return true;
            }
        }
        current = parent;
    }
    false
}

fn in_type_arguments(node: tree_sitter::Node<'_>) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "type_arguments" {
            return true;
        }
        current = parent;
    }
    false
}

fn in_const_value_position(node: tree_sitter::Node<'_>) -> bool {
    in_array_length(node) || in_type_arguments(node)
}

fn in_const_lifetime_position(node: tree_sitter::Node<'_>) -> bool {
    if in_array_length(node) {
        return true;
    }
    let mut current = node;
    while let Some(parent) = current.parent() {
        if parent.kind() == "type_arguments" && matches!(current.kind(), "block" | "const_block") {
            return true;
        }
        current = parent;
    }
    false
}

/// Method name of `self.foo()`, `Self::foo()`, `S::foo()` inside `impl S`, or JS
/// `self.foo()` / `Self::foo` / `Self::N` / `this.foo()` — bound to a sibling
/// under the enclosing impl/class, not the flat Value env (a free `fn foo` is
/// a different target). `x.foo()` stays Free: that needs types.
fn is_inherent_method_ref(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    match node.kind() {
        "field_identifier" => rust_self_field_call(node),
        "property_identifier" => js_this_member_call(node),
        "identifier" => rust_self_path_method(node, src),
        _ => false,
    }
}

fn rust_self_field_call(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent().filter(|p| p.kind() == "field_expression") else {
        return false;
    };
    if parent.child_by_field_name("field").map(|n| n.id()) != Some(node.id()) {
        return false;
    }
    let Some(value) = parent.child_by_field_name("value") else {
        return false;
    };
    if value.kind() != "self" {
        return false;
    }
    let Some(grand) = parent.parent() else {
        return false;
    };
    grand.kind() == "call_expression"
        && grand.child_by_field_name("function").map(|n| n.id()) == Some(parent.id())
}

fn js_this_member_call(node: tree_sitter::Node<'_>) -> bool {
    let Some(parent) = node.parent().filter(|p| p.kind() == "member_expression") else {
        return false;
    };
    if parent.child_by_field_name("property").map(|n| n.id()) != Some(node.id()) {
        return false;
    }
    let Some(object) = parent.child_by_field_name("object") else {
        return false;
    };
    if object.kind() != "this" {
        return false;
    }
    let Some(grand) = parent.parent() else {
        return false;
    };
    grand.kind() == "call_expression"
        && grand.child_by_field_name("function").map(|n| n.id()) == Some(parent.id())
}

fn rust_self_path_method(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    let Some(parent) = node.parent().filter(|p| p.kind() == "scoped_identifier") else {
        return false;
    };
    if parent.child_by_field_name("name").map(|n| n.id()) != Some(node.id()) {
        return false;
    }
    let Some(path) = parent.child_by_field_name("path") else {
        return false;
    };
    let Some(qual) = scoped_qualifier(path) else {
        return false;
    };
    let start = qual.start_byte();
    let end = qual.end_byte();
    if end > src.len() || start >= end {
        return false;
    }
    let path_text = &src[start..end];
    if path_text != b"Self"
        && Some(path_text) != enclosing_impl_type_name(node, src)
        && Some(path_text) != enclosing_impl_trait_name(node, src)
    {
        return false;
    }
    true
}

/// The `Self` type of the enclosing `impl` (`S` in `impl S` / `impl Trait for S` /
/// `impl<T> S<T>`), or the enclosing trait name (`T` in `trait T`). Used so
/// `S::foo()` / `T::Item` bind like `Self::foo()` / `Self::Item`.
fn enclosing_impl_type_name<'a>(
    mut node: tree_sitter::Node<'a>,
    src: &'a [u8],
) -> Option<&'a [u8]> {
    loop {
        let Some(parent) = node.parent() else {
            return None;
        };
        if parent.kind() == "impl_item" {
            return parent
                .child_by_field_name("type")
                .and_then(|ty| impl_type_base_name(ty, src));
        }
        if parent.kind() == "trait_item" {
            return parent
                .child_by_field_name("name")
                .and_then(|n| impl_type_base_name(n, src));
        }
        node = parent;
    }
}

/// The trait in `impl Trait for S`, so `Tr::Item` / `Tr::foo` inside that impl
/// bind like `Self::Item` / `Self::foo`.
fn enclosing_impl_trait_name<'a>(
    mut node: tree_sitter::Node<'a>,
    src: &'a [u8],
) -> Option<&'a [u8]> {
    loop {
        let Some(parent) = node.parent() else {
            return None;
        };
        if parent.kind() == "impl_item" {
            return parent
                .child_by_field_name("trait")
                .and_then(|ty| impl_type_base_name(ty, src));
        }
        node = parent;
    }
}

fn impl_type_base_name<'a>(ty: tree_sitter::Node<'a>, src: &'a [u8]) -> Option<&'a [u8]> {
    match ty.kind() {
        "type_identifier" | "identifier" => {
            let start = ty.start_byte();
            let end = ty.end_byte();
            (end <= src.len() && start < end).then(|| &src[start..end])
        }
        "generic_type" => ty
            .child_by_field_name("type")
            .and_then(|inner| impl_type_base_name(inner, src)),
        "scoped_type_identifier" => ty
            .child_by_field_name("name")
            .and_then(|inner| impl_type_base_name(inner, src)),
        _ => None,
    }
}

fn is_doc_attribute(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    let Some(attr) = node.named_child(0).filter(|n| n.kind() == "attribute") else {
        return false;
    };
    let Some(path) = attr.named_child(0) else {
        return false;
    };
    &src[path.start_byte()..path.end_byte()] == b"doc"
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
    if matches!(node.kind(), "attribute_item" | "inner_attribute_item") {
        if !is_doc_attribute(node, src) {
            let r = byte_range(node);
            let text = String::from_utf8_lossy(&src[r.start as usize..r.end as usize]);
            if !text.trim().is_empty() {
                tokens.push(Token::Lit(text.as_ref().into()));
            }
        }
        return;
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
        if let Some(tok) = leaf_token(node, src, item, lang, res, binders) {
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
    lang: &dyn Lang,
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
    if lang.literal_kinds().contains(&kind) {
        Some(Token::Lit(text.as_ref().into()))
    } else if kind.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
        Some(Token::Kw(text.as_ref().into()))
    } else {
        Some(Token::Punct(text.as_ref().into()))
    }
}
