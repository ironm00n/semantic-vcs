use std::cell::RefCell;
use std::path::Path;
use std::sync::OnceLock;

use tree_sitter_highlight::{Highlight, HighlightConfiguration, Highlighter, HtmlRenderer};

const RUST_LOCALS_QUERY: &str = include_str!("../queries/rust/locals.scm");

const CAPTURES: &[&str] = &[
    "attribute",
    "boolean",
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
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.member",
    "variable.parameter",
];

thread_local! {
    static RENDERER: RefCell<(Highlighter, HtmlRenderer)> = RefCell::new((Highlighter::new(), HtmlRenderer::new()));
}

pub fn source_html(source: &str, file: &str) -> Option<String> {
    let configuration = configuration(file)?;
    RENDERER.with(|state| {
        let (highlighter, renderer) = &mut *state.borrow_mut();
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
            .ok()?;
        renderer.reset();
        renderer
            .render(events, source.as_bytes(), &|Highlight(index), output| {
                output.extend_from_slice(
                    format!(
                        "class=\"syntax syntax-{}\"",
                        CAPTURES[index].replace('.', "-")
                    )
                    .as_bytes(),
                );
            })
            .ok()?;
        String::from_utf8(renderer.html.clone()).ok()
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_source_is_escaped_and_highlighted() {
        let html = source_html(
            "fn parse<T>(value: T) -> bool { value < value }",
            "src/lib.rs",
        )
        .unwrap();
        assert!(html.contains("syntax-keyword\">fn</span>"), "{html}");
        assert!(html.contains("syntax-type\">T</span>"), "{html}");
        assert!(
            html.matches("syntax-variable-parameter").count() >= 3,
            "{html}"
        );
        assert!(html.contains("&lt;"), "{html}");
    }

    #[test]
    fn javascript_and_jsx_are_highlighted() {
        let html = source_html("export const View = () => <main>ok</main>", "view.jsx").unwrap();
        assert!(html.contains("syntax-keyword\">export</span>"), "{html}");
        assert!(html.contains("syntax-tag\">main</span>"), "{html}");
    }

    #[test]
    fn unsupported_source_has_no_highlighted_html() {
        assert!(source_html("name = 'svc'", "Cargo.toml").is_none());
    }
}
