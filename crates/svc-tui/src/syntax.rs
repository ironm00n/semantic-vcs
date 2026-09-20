use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

const RUST_LOCALS_QUERY: &str = include_str!("../queries/rust/locals.scm");

const CAPTURES: &[&str] = &[
    "attribute",
    "boolean",
    "carriage-return",
    "comment",
    "comment.documentation",
    "constant",
    "constant.builtin",
    "constructor",
    "constructor.builtin",
    "embedded",
    "error",
    "escape",
    "function",
    "function.builtin",
    "keyword",
    "label",
    "markup",
    "markup.bold",
    "markup.heading",
    "markup.italic",
    "markup.link",
    "markup.link.url",
    "markup.list",
    "markup.list.checked",
    "markup.list.numbered",
    "markup.list.unchecked",
    "markup.list.unnumbered",
    "markup.quote",
    "markup.raw",
    "markup.raw.block",
    "markup.raw.inline",
    "markup.strikethrough",
    "module",
    "number",
    "operator",
    "property",
    "property.builtin",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation.special",
    "string",
    "string.escape",
    "string.regexp",
    "string.special",
    "string.special.symbol",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.member",
    "variable.parameter",
];

thread_local! {
    static HIGHLIGHTER: RefCell<Highlighter> = RefCell::new(Highlighter::new());
}

pub fn source_lines(source: &str, file: &str) -> Vec<Line<'static>> {
    let Some(config) = configuration(file) else {
        return plain_lines(source);
    };
    highlighted_lines(source, config).unwrap_or_else(|| plain_lines(source))
}

fn configuration(file: &str) -> Option<&'static HighlightConfiguration> {
    match Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("rs") => Some(rust_configuration()),
        Some("js" | "jsx" | "mjs" | "cjs") => Some(javascript_configuration()),
        _ => None,
    }
}

fn rust_configuration() -> &'static HighlightConfiguration {
    static CONFIGURATION: OnceLock<HighlightConfiguration> = OnceLock::new();
    CONFIGURATION.get_or_init(|| {
        let highlights = format!(
            "(identifier) @variable\n{}",
            tree_sitter_rust::HIGHLIGHTS_QUERY
        );
        let mut configuration = HighlightConfiguration::new(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            &highlights,
            tree_sitter_rust::INJECTIONS_QUERY,
            RUST_LOCALS_QUERY,
        )
        .expect("tree-sitter-rust highlight queries");
        configuration.configure(CAPTURES);
        configuration
    })
}

fn javascript_configuration() -> &'static HighlightConfiguration {
    static CONFIGURATION: OnceLock<HighlightConfiguration> = OnceLock::new();
    CONFIGURATION.get_or_init(|| {
        let highlights = format!(
            "{}\n{}",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
        );
        let mut configuration = HighlightConfiguration::new(
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
            &highlights,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        )
        .expect("tree-sitter-javascript highlight queries");
        configuration.configure(CAPTURES);
        configuration
    })
}

fn highlighted_lines(
    source: &str,
    configuration: &'static HighlightConfiguration,
) -> Option<Vec<Line<'static>>> {
    HIGHLIGHTER.with(|highlighter| {
        let mut highlighter = highlighter.borrow_mut();
        let events = highlighter
            .highlight(
                configuration,
                source.as_bytes(),
                None,
                None,
                |language| match language {
                    "rust" => Some(rust_configuration()),
                    "javascript" | "js" | "jsx" => Some(javascript_configuration()),
                    _ => None,
                },
            )
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        render_events(source, events)
    })
}

fn render_events(source: &str, events: Vec<HighlightEvent>) -> Option<Vec<Line<'static>>> {
    let mut lines = Vec::new();
    let mut spans = Vec::new();
    let mut styles = Vec::new();
    for event in events {
        match event {
            HighlightEvent::HighlightStart(highlight) => styles.push(style(highlight)),
            HighlightEvent::HighlightEnd => {
                styles.pop()?;
            }
            HighlightEvent::Source { start, end } => {
                let text = source.get(start..end)?;
                push_text(
                    &mut lines,
                    &mut spans,
                    text,
                    styles.last().copied().unwrap_or_default(),
                );
            }
        }
    }
    if !styles.is_empty() {
        return None;
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from(spans));
    }
    Some(lines)
}

fn push_text(
    lines: &mut Vec<Line<'static>>,
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
) {
    for part in text.split_inclusive('\n') {
        let content = part.strip_suffix('\n').unwrap_or(part);
        if !content.is_empty() {
            spans.push(Span::styled(content.to_string(), style));
        }
        if part.ends_with('\n') {
            lines.push(Line::from(std::mem::take(spans)));
        }
    }
}

fn plain_lines(source: &str) -> Vec<Line<'static>> {
    let mut lines = source
        .lines()
        .map(|line| Line::from(line.to_string()))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(Line::default());
    }
    lines
}

fn style(Highlight(index): Highlight) -> Style {
    let name = CAPTURES[index];
    match name {
        "attribute" => Style::default().fg(Color::Yellow),
        "boolean" | "constant" | "constant.builtin" | "number" => {
            Style::default().fg(Color::LightYellow)
        }
        "comment" | "comment.documentation" => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        "constructor" | "constructor.builtin" | "type" | "type.builtin" => {
            Style::default().fg(Color::LightCyan)
        }
        "error" => Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::UNDERLINED),
        "escape" | "string.escape" => Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD),
        "function" | "function.builtin" => Style::default().fg(Color::LightBlue),
        "keyword" => Style::default()
            .fg(Color::LightMagenta)
            .add_modifier(Modifier::BOLD),
        "label" => Style::default().fg(Color::LightYellow),
        "module" => Style::default().fg(Color::Cyan),
        "operator" => Style::default().fg(Color::LightMagenta),
        "property" | "property.builtin" | "variable.member" => Style::default().fg(Color::Cyan),
        "punctuation" | "punctuation.bracket" | "punctuation.delimiter" | "punctuation.special" => {
            Style::default().fg(Color::Gray)
        }
        "string" | "string.regexp" | "string.special" | "string.special.symbol" => {
            Style::default().fg(Color::Green)
        }
        "tag" => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        "variable.builtin" => Style::default().fg(Color::LightRed),
        "variable.parameter" => Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::ITALIC),
        _ => Style::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_uses_grammar_highlights_without_changing_source() {
        let source = "pub fn greet(name: &str) -> usize { // hello\n    name.len() + 1\n}";
        let lines = source_lines(source, "src/lib.rs");

        assert_eq!(text(&lines), source);
        assert_eq!(foreground(&lines, "fn"), Some(Color::LightMagenta));
        assert_eq!(foreground(&lines, "greet"), Some(Color::LightBlue));
        assert_eq!(foreground(&lines, "str"), Some(Color::LightCyan));
        assert_eq!(foreground(&lines, "// hello"), Some(Color::DarkGray));
        assert_eq!(
            foregrounds(&lines, "name"),
            vec![Some(Color::LightCyan), Some(Color::LightCyan)]
        );
    }

    #[test]
    fn rust_injections_highlight_macro_token_trees() {
        let lines = source_lines(
            "fn f(value: i32) { println!(\"{}\", value + 1); }",
            "src/main.rs",
        );

        assert_eq!(foreground(&lines, "1"), Some(Color::LightYellow));
        assert_eq!(foreground(&lines, "\"{}\""), Some(Color::Green));
    }

    #[test]
    fn javascript_and_jsx_use_the_shipped_queries() {
        let source =
            "export function renderCard() { return <section className=\"card\">ok</section>; }";
        let lines = source_lines(source, "src/card.jsx");

        assert_eq!(text(&lines), source);
        assert_eq!(foreground(&lines, "export"), Some(Color::LightMagenta));
        assert_eq!(foreground(&lines, "renderCard"), Some(Color::LightBlue));
        assert_eq!(foreground(&lines, "\"card\""), Some(Color::Green));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .filter(|span| span.style.fg.is_some())
                .count()
                >= 6
        );
    }

    #[test]
    fn javascript_locals_do_not_receive_builtin_highlights() {
        let local = source_lines("function f(console) { return console; }", "src/main.js");
        let builtin = source_lines("console.log('ready');", "src/main.js");

        assert_eq!(foregrounds(&local, "console"), vec![None, None]);
        assert_eq!(foreground(&builtin, "console"), Some(Color::LightRed));
    }

    #[test]
    fn unsupported_files_remain_plaintext() {
        let lines = source_lines("key = \"value\"\nnext", "Config.toml");

        assert_eq!(text(&lines), "key = \"value\"\nnext");
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style == Style::default())
        );
    }

    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn foreground(lines: &[Line<'_>], content: &str) -> Option<Color> {
        foregrounds(lines, content).into_iter().next().flatten()
    }

    fn foregrounds(lines: &[Line<'_>], content: &str) -> Vec<Option<Color>> {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.content == content)
            .map(|span| span.style.fg)
            .collect()
    }
}
