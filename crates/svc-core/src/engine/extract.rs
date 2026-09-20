use crate::entity::Kind;
use crate::error::Result;
use crate::ids::ByteRange;
use crate::lang::{EntityKindRule, Lang, RawEntity};

pub fn extract(tree: &tree_sitter::Tree, src: &[u8], lang: &dyn Lang) -> Result<Vec<RawEntity>> {
    let mut raw = Vec::new();
    let mut nodes = Vec::new();
    collect(tree.root_node(), src, lang, None, &mut raw, &mut nodes);
    assign_extents(&mut raw, &nodes, src);
    Ok(raw)
}

fn rule_for<'a>(lang: &'a dyn Lang, kind: &str) -> Option<&'a EntityKindRule> {
    lang.entity_kinds().iter().find(|r| r.node_kind == kind)
}

fn is_class_body_member(node: tree_sitter::Node<'_>) -> bool {
    node.parent().is_some_and(|p| p.kind() == "class_body")
}

fn is_module_scope_declarator(node: tree_sitter::Node<'_>) -> bool {
    let mut p = node.parent();
    while let Some(parent) = p {
        match parent.kind() {
            "program" => return true,
            "export_statement"
            | "lexical_declaration"
            | "variable_declaration"
            | "using_declaration" => p = parent.parent(),
            _ => return false,
        }
    }
    false
}

pub fn find_node<'a>(
    node: tree_sitter::Node<'a>,
    range: ByteRange,
) -> Option<tree_sitter::Node<'a>> {
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if (ch.start_byte() as u32) <= range.start
            && (ch.end_byte() as u32) >= range.end
            && let Some(hit) = find_node(ch, range)
        {
            return Some(hit);
        }
    }
    (byte_range(node) == range).then_some(node)
}

/// `export default function () {}` / `() => {}` / `class { }` parse as
/// expressions, not declarations, so they are missing from `entity_kinds`.
/// Without this they vanish (or a class method is promoted to a file root).
fn default_export_expression_kind(node: tree_sitter::Node<'_>) -> Option<Kind> {
    if !node.parent().is_some_and(|p| p.kind() == "export_statement") {
        return None;
    }
    match node.kind() {
        "function_expression" | "generator_function" | "arrow_function" => Some(Kind::JsFunction),
        "class" => Some(Kind::JsClass),
        _ => None,
    }
}

fn node_text(src: &[u8], node: tree_sitter::Node<'_>) -> String {
    String::from_utf8_lossy(&src[node.start_byte()..node.end_byte()]).into_owned()
}

fn parse_path_lit(lit: &str) -> Option<String> {
    let lit = lit.trim();
    if let Some(inner) = lit.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        return nonempty_path(inner);
    }
    parse_raw_string(lit)
}

fn parse_raw_string(lit: &str) -> Option<String> {
    let rest = lit.strip_prefix('r')?;
    let hashes = rest.chars().take_while(|&c| c == '#').count();
    let rest = rest.get(hashes..)?;
    let rest = rest.strip_prefix('"')?;
    let suffix = format!("\"{}", "#".repeat(hashes));
    nonempty_path(rest.strip_suffix(&suffix)?)
}

fn nonempty_path(inner: &str) -> Option<String> {
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

fn path_from_attr_token_tree(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    let mut c = node.walk();
    let kids: Vec<_> = node.children(&mut c).collect();
    let mut i = 0;
    if i < kids.len() && kids[i].kind() == "[" {
        i += 1;
    }
    if i >= kids.len() || kids[i].kind() != "identifier" || node_text(src, kids[i]) != "path" {
        return None;
    }
    i += 1;
    if i < kids.len() && kids[i].kind() == "=" {
        i += 1;
    }
    if i >= kids.len() {
        return None;
    }
    match kids[i].kind() {
        "string_literal" | "raw_string_literal" => parse_path_lit(&node_text(src, kids[i])),
        _ => None,
    }
}

fn path_attr_of(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<String> {
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" => {
                if let Some(s) = path_eq_literal(&node_text(src, p)) {
                    return Some(s);
                }
            }
            "visibility_modifier" | "pub" => {}
            // `m! { #[path = "bar.rs"] mod foo; }` — the grammar leaves
            // `#[path = …]` as `#` plus a token_tree, not `attribute_item`.
            "token_tree" => {
                if let Some(s) = path_from_attr_token_tree(p, src) {
                    return Some(s);
                }
            }
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if ch.kind() == "attribute_item" {
            if let Some(s) = path_eq_literal(&node_text(src, ch)) {
                return Some(s);
            }
        }
    }
    None
}

fn path_eq_literal(attr: &str) -> Option<String> {
    let rest = attr.trim().strip_prefix("#[")?.strip_suffix(']')?.trim();
    let rest = rest.strip_prefix("path")?.trim();
    let rest = rest.strip_prefix('=')?.trim();
    parse_path_lit(rest)
}

fn attr_is_macro_export(attr: &str) -> bool {
    let inner = attr
        .trim()
        .strip_prefix("#[")
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(attr.trim())
        .trim();
    inner == "macro_export" || inner.starts_with("macro_export(")
}

fn token_tree_is_macro_export(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    let text = node_text(src, node);
    let t = text.trim();
    t == "[macro_export]" || t.starts_with("[macro_export(")
}

fn has_macro_export(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    let mut prev = node.prev_named_sibling();
    while let Some(p) = prev {
        match p.kind() {
            "attribute_item" if attr_is_macro_export(&node_text(src, p)) => return true,
            "visibility_modifier" | "pub" => {}
            "token_tree" if token_tree_is_macro_export(p, src) => return true,
            _ => break,
        }
        prev = p.prev_named_sibling();
    }
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if ch.kind() == "attribute_item" && attr_is_macro_export(&node_text(src, ch)) {
            return true;
        }
    }
    false
}

fn preceding_macro_export(kids: &[tree_sitter::Node<'_>], f: usize, src: &[u8]) -> bool {
    let mut k = f;
    while k > 0 {
        k -= 1;
        match kids[k].kind() {
            "pub" | "visibility_modifier" | "!" | "#" => {}
            "attribute_item" if attr_is_macro_export(&node_text(src, kids[k])) => return true,
            "token_tree" if token_tree_is_macro_export(kids[k], src) => return true,
            _ => break,
        }
    }
    false
}

fn mark_macro_export(raw: &mut [RawEntity], export: bool) {
    if export && let Some(ent) = raw.last_mut() {
        if ent.kind == Kind::Macro {
            ent.macro_export = true;
        }
    }
}

fn emit<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    parent_idx: Option<usize>,
    kind: Kind,
    name: String,
    name_range: Option<ByteRange>,
    raw: &mut Vec<RawEntity>,
    nodes: &mut Vec<tree_sitter::Node<'a>>,
) {
    let idx = raw.len();
    if let Some(p) = parent_idx {
        raw[p].children.push(idx);
    }
    raw.push(RawEntity {
        kind,
        name,
        name_range,
        item_range: byte_range(node),
        bytes_range: byte_range(node),
        parent_idx,
        children: Vec::new(),
        path_attr: None,
        macro_export: false,
    });
    nodes.push(node);
    if kind == Kind::Mod {
        if let Some(p) = path_attr_of(node, src) {
            raw[idx].path_attr = Some(p);
        }
    }
    if kind == Kind::Macro {
        raw[idx].macro_export = has_macro_export(node, src);
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, src, lang, Some(idx), raw, nodes);
    }
}

fn collect<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    parent_idx: Option<usize>,
    raw: &mut Vec<RawEntity>,
    nodes: &mut Vec<tree_sitter::Node<'a>>,
) {
    if let Some(kind) = default_export_expression_kind(node) {
        let name_node = node.child_by_field_name("name");
        let name = name_node
            .map(|n| node_text(src, n))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".into());
        emit(
            node,
            src,
            lang,
            parent_idx,
            kind,
            name,
            name_node.map(byte_range),
            raw,
            nodes,
        );
        return;
    }
    if let Some(rule) = rule_for(lang, node.kind()) {
        // Module-scope `let`/`const`/`var` are entities; nested ones are locals
        // of the enclosing item. Extracting them as children made the
        // parent resolver skip their binders. `for (let i …)` is also a local:
        // `for_statement` is not an entity, so parent_idx is None, but the
        // declarator is not a file-root.
        if rule.node_kind == "variable_declarator"
            && (parent_idx.is_some() || !is_module_scope_declarator(node))
        {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect(child, src, lang, parent_idx, raw, nodes);
            }
            return;
        }
        // Object-literal `{ async execute() {} }` is not a class member. Treating
        // it as `JsMethod` under the enclosing function made two `execute`
        // methods share a SigKey and every merge of svc's own harness AddAdd.
        if rule.node_kind == "method_definition" && !is_class_body_member(node) {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect(child, src, lang, parent_idx, raw, nodes);
            }
            return;
        }
        let mut name = lang.entity_name(node, src).unwrap_or_default();
        if name.is_empty() {
            name = format!("«{}:{}»", node.kind(), node.start_byte());
        }
        let name_range = rule
            .name_field
            .and_then(|f| node.child_by_field_name(f))
            .map(byte_range);
        emit(
            node,
            src,
            lang,
            parent_idx,
            lang.refine_kind(node, src).unwrap_or(rule.kind),
            name,
            name_range,
            raw,
            nodes,
        );
        return;
    }
    if lang.name() == "rust" && node.kind() == "token_tree" {
        let under_macro_def = parent_idx.is_some_and(|p| raw[p].kind == Kind::Macro);
        if !under_macro_def {
            collect_macro_mod_decls(node, src, lang, parent_idx, raw, nodes);
            return;
        }
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, src, lang, parent_idx, raw, nodes);
    }
}

/// `cfg_fs! { pub mod fs; }` does not parse a `mod_item` — the grammar leaves
/// `pub`/`mod`/`fs`/`;` as token-tree children. File-root invocations mint the
/// Mod so `src/fs/` attaches. The same soup inside `mod outer { … }` must mint
/// a child of that module so `src/outer/fs.rs` attaches. Brace-body
/// `pub mod fs { pub fn parse() {} }` has no file; mint the Mod and its `fn`
/// children so `crate::fs::parse` walks. `struct`/`const`/`type` and the other
/// named items in that soup are minted the same way, as is `macro_rules!` and
/// macros 2.0 `macro name`. `pub use` is minted Opaque so the existing nested-use
/// pass binds it as a reexport. Token trees under a
/// `macro_definition` stay matcher/body, not declarations.
fn collect_macro_mod_decls<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    parent_idx: Option<usize>,
    raw: &mut Vec<RawEntity>,
    nodes: &mut Vec<tree_sitter::Node<'a>>,
) {
    let mut c = node.walk();
    let kids: Vec<_> = node.children(&mut c).collect();
    let mut i = 0;
    while i < kids.len() {
        if kids[i].kind() == "token_tree" {
            collect_macro_mod_decls(kids[i], src, lang, parent_idx, raw, nodes);
            i += 1;
            continue;
        }
        let mut j = i;
        while j < kids.len() && kids[j].kind() == "attribute_item" {
            j += 1;
        }
        if j < kids.len() && matches!(kids[j].kind(), "pub" | "visibility_modifier") {
            j += 1;
        }
        if j + 2 < kids.len()
            && kids[j].kind() == "mod"
            && kids[j + 1].kind() == "identifier"
            && kids[j + 2].kind() == ";"
        {
            let name_node = kids[j + 1];
            let name = node_text(src, name_node);
            emit(
                name_node,
                src,
                lang,
                parent_idx,
                Kind::Mod,
                name,
                Some(byte_range(name_node)),
                raw,
                nodes,
            );
            i = j + 3;
            continue;
        }
        if j + 2 < kids.len()
            && kids[j].kind() == "mod"
            && kids[j + 1].kind() == "identifier"
            && kids[j + 2].kind() == "token_tree"
        {
            let name_node = kids[j + 1];
            let name = node_text(src, name_node);
            emit(
                name_node,
                src,
                lang,
                parent_idx,
                Kind::Mod,
                name,
                Some(byte_range(name_node)),
                raw,
                nodes,
            );
            let mod_idx = raw.len() - 1;
            collect_macro_mod_decls(kids[j + 2], src, lang, Some(mod_idx), raw, nodes);
            i = j + 3;
            continue;
        }
        if j < kids.len() && kids[j].kind() == "use" {
            let mut e = j + 1;
            while e < kids.len() && kids[e].kind() != ";" {
                e += 1;
            }
            if e < kids.len() {
                if let Some(&name_node) = kids[j..=e]
                    .iter()
                    .rev()
                    .find(|n| n.kind() == "identifier")
                {
                    let start = if j > 0
                        && matches!(kids[j - 1].kind(), "pub" | "visibility_modifier")
                    {
                        kids[j - 1].start_byte()
                    } else {
                        kids[j].start_byte()
                    };
                    let name =
                        String::from_utf8_lossy(&src[start..kids[e].end_byte()]).into_owned();
                    emit(
                        name_node,
                        src,
                        lang,
                        parent_idx,
                        Kind::Opaque,
                        name,
                        Some(byte_range(name_node)),
                        raw,
                        nodes,
                    );
                    i = e + 1;
                    continue;
                }
            }
        }
        let mut f = i;
        while f < kids.len() && is_macro_item_prefix(kids[f]) {
            f += 1;
        }
        let mut q = f;
        while q < kids.len() && is_macro_fn_qualifier(kids[q]) {
            q += 1;
        }
        if q + 1 < kids.len() && kids[q].kind() == "fn" && kids[q + 1].kind() == "identifier" {
            emit_macro_named(
                kids[q + 1],
                src,
                lang,
                parent_idx,
                Kind::Fn,
                raw,
                nodes,
            );
            i = q + 2;
            continue;
        }
        if f + 1 < kids.len()
            && kids[f + 1].kind() == "identifier"
            && let Some(kind) = macro_named_item_kind(kids[f].kind())
        {
            emit_macro_named(kids[f + 1], src, lang, parent_idx, kind, raw, nodes);
            i = f + 2;
            continue;
        }
        if is_macro_rules_kw(kids[f], src) {
            let mut m = f + 1;
            if m < kids.len() && kids[m].kind() == "!" {
                m += 1;
            }
            if m < kids.len() && kids[m].kind() == "identifier" {
                emit_macro_named(
                    kids[m],
                    src,
                    lang,
                    parent_idx,
                    Kind::Macro,
                    raw,
                    nodes,
                );
                mark_macro_export(raw, preceding_macro_export(&kids, f, src));
                i = m + 1;
                if i < kids.len() && kids[i].kind() == "token_tree" {
                    i += 1;
                }
                if i < kids.len() && kids[i].kind() == ";" {
                    i += 1;
                }
                continue;
            }
        }
        if is_macro_kw(kids[f], src)
            && f + 1 < kids.len()
            && kids[f + 1].kind() == "identifier"
        {
            emit_macro_named(
                kids[f + 1],
                src,
                lang,
                parent_idx,
                Kind::Macro,
                raw,
                nodes,
            );
            mark_macro_export(raw, preceding_macro_export(&kids, f, src));
            i = f + 2;
            while i < kids.len() && kids[i].kind() == "token_tree" {
                i += 1;
            }
            if i < kids.len() && kids[i].kind() == ";" {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
}

fn is_macro_rules_kw(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    node.kind() == "macro_rules" || node_text(src, node) == "macro_rules"
}

fn is_macro_kw(node: tree_sitter::Node<'_>, src: &[u8]) -> bool {
    !is_macro_rules_kw(node, src)
        && (node.kind() == "macro" || node_text(src, node) == "macro")
}

fn is_macro_item_prefix(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "attribute_item" | "pub" | "visibility_modifier"
    )
}

fn is_macro_fn_qualifier(node: tree_sitter::Node<'_>) -> bool {
    matches!(
        node.kind(),
        "async" | "const" | "unsafe" | "extern" | "string_literal" | "raw_string_literal"
    )
}

fn macro_named_item_kind(kw: &str) -> Option<Kind> {
    Some(match kw {
        "struct" => Kind::Struct,
        "enum" => Kind::Enum,
        "union" => Kind::Union,
        "trait" => Kind::Trait,
        "type" => Kind::TypeAlias,
        "const" => Kind::Const,
        "static" => Kind::Static,
        _ => return None,
    })
}

fn emit_macro_named<'a>(
    name_node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    parent_idx: Option<usize>,
    kind: Kind,
    raw: &mut Vec<RawEntity>,
    nodes: &mut Vec<tree_sitter::Node<'a>>,
) {
    let name = node_text(src, name_node);
    emit(
        name_node,
        src,
        lang,
        parent_idx,
        kind,
        name,
        Some(byte_range(name_node)),
        raw,
        nodes,
    );
}

fn body_first_byte(node: tree_sitter::Node<'_>, src: &[u8]) -> Option<u32> {
    let body = node.child_by_field_name("body")?;
    let start = body.start_byte();
    let extra = if src.get(start) == Some(&b'{') { 1 } else { 0 };
    Some(start as u32 + extra)
}

fn assign_extents(raw: &mut [RawEntity], nodes: &[tree_sitter::Node<'_>], src: &[u8]) {
    let n = raw.len();
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); n + 1];
    for i in 0..n {
        let g = raw[i].parent_idx.map(|p| p + 1).unwrap_or(0);
        groups[g].push(i);
    }
    for g in &mut groups {
        g.sort_by_key(|&i| raw[i].item_range.start);
    }
    fill(raw, nodes, &groups, 0, src);
}

fn fill(
    raw: &mut [RawEntity],
    nodes: &[tree_sitter::Node<'_>],
    groups: &[Vec<usize>],
    g: usize,
    src: &[u8],
) {
    let sibs = groups[g].clone();
    for (k, &i) in sibs.iter().enumerate() {
        let start = if k == 0 {
            if g == 0 {
                0
            } else {
                body_first_byte(nodes[g - 1], src).unwrap_or(raw[i].item_range.start)
            }
        } else {
            raw[sibs[k - 1]].bytes_range.end
        };
        // Last file-root owns following kindless statements (`for`, expr
        // stmts). Stop before trailing whitespace so the close-brace literal
        // stays `}\n` in FileRecord.trailing — nested add_def splices there.
        let mut end = raw[i].item_range.end;
        if g == 0 && k + 1 == sibs.len() {
            let mut e = src.len();
            while e > end as usize && src[e - 1].is_ascii_whitespace() {
                e -= 1;
            }
            end = e as u32;
        }
        raw[i].bytes_range = ByteRange {
            start: start.min(raw[i].item_range.start),
            end: end.max(raw[i].item_range.end),
        };
        fill(raw, nodes, groups, i + 1, src);
    }
}

/// True when `collect` would emit this node as an entity (same skip rules:
/// nested `variable_declarator` stays a local; object-literal methods are not
/// class members).
pub fn is_extracted_item(node: tree_sitter::Node<'_>, lang: &dyn Lang) -> bool {
    if default_export_expression_kind(node).is_some() {
        return true;
    }
    let Some(rule) = rule_for(lang, node.kind()) else {
        return false;
    };
    if rule.node_kind == "variable_declarator" {
        return is_module_scope_declarator(node);
    }
    if rule.node_kind == "method_definition" {
        return is_class_body_member(node);
    }
    true
}

pub fn byte_range(node: tree_sitter::Node<'_>) -> ByteRange {
    ByteRange {
        start: node.start_byte() as u32,
        end: node.end_byte() as u32,
    }
}
