use crate::error::Result;
use crate::ids::ByteRange;
use crate::lang::{EntityKindRule, Lang, RawEntity};

pub fn extract(
    tree: &tree_sitter::Tree,
    src: &[u8],
    lang: &dyn Lang,
) -> Result<Vec<RawEntity>> {
    let mut raw = Vec::new();
    let mut nodes = Vec::new();
    collect(tree.root_node(), src, lang, None, &mut raw, &mut nodes);
    assign_extents(&mut raw, &nodes, src);
    Ok(raw)
}

fn rule_for<'a>(lang: &'a dyn Lang, kind: &str) -> Option<&'a EntityKindRule> {
    lang.entity_kinds().iter().find(|r| r.node_kind == kind)
}

pub fn find_node<'a>(
    node: tree_sitter::Node<'a>,
    range: ByteRange,
) -> Option<tree_sitter::Node<'a>> {
    let mut c = node.walk();
    for ch in node.named_children(&mut c) {
        if (ch.start_byte() as u32) <= range.start && (ch.end_byte() as u32) >= range.end {
            if let Some(hit) = find_node(ch, range) {
                return Some(hit);
            }
        }
    }
    (byte_range(node) == range).then_some(node)
}

fn collect<'a>(
    node: tree_sitter::Node<'a>,
    src: &[u8],
    lang: &dyn Lang,
    parent_idx: Option<usize>,
    raw: &mut Vec<RawEntity>,
    nodes: &mut Vec<tree_sitter::Node<'a>>,
) {
    if let Some(rule) = rule_for(lang, node.kind()) {
        // Module-scope `let`/`const`/`var` are entities; nested ones are locals
        // of the enclosing item (design §9). Extracting them as children made the
        // parent resolver skip their binders.
        if rule.node_kind == "variable_declarator" && parent_idx.is_some() {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                collect(child, src, lang, parent_idx, raw, nodes);
            }
            return;
        }
        let idx = raw.len();
        if let Some(p) = parent_idx {
            raw[p].children.push(idx);
        }
        let mut name = lang.entity_name(node, src).unwrap_or_default();
        if name.is_empty() {
            name = format!("«{}:{}»", node.kind(), node.start_byte());
        }
        let name_range = rule
            .name_field
            .and_then(|f| node.child_by_field_name(f))
            .map(byte_range);
        raw.push(RawEntity {
            kind: lang.refine_kind(node, src).unwrap_or(rule.kind),
            name,
            name_range,
            item_range: byte_range(node),
            bytes_range: byte_range(node),
            parent_idx,
            children: Vec::new(),
        });
        nodes.push(node);
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(child, src, lang, Some(idx), raw, nodes);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect(child, src, lang, parent_idx, raw, nodes);
    }
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
        raw[i].bytes_range = ByteRange {
            start: start.min(raw[i].item_range.start),
            end: raw[i].item_range.end,
        };
        fill(raw, nodes, groups, i + 1, src);
    }
}

pub fn byte_range(node: tree_sitter::Node<'_>) -> ByteRange {
    ByteRange {
        start: node.start_byte() as u32,
        end: node.end_byte() as u32,
    }
}
