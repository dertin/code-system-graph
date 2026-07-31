use thiserror::Error;
use tree_sitter::{Language, Parser};

/// Source grammar selected for focused boundary inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceSyntaxLanguage {
    /// JavaScript or JSX.
    JavaScript,
    /// TypeScript or TSX.
    TypeScript,
    /// Rust.
    Rust,
    /// Python.
    Python,
    /// Go.
    Go,
    /// Java.
    Java,
}

/// Bounded syntax inspection used to corroborate focused textual facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSyntaxInspection {
    /// Whether Tree-sitter recovered from at least one syntax error.
    pub has_error: bool,
    /// Number of syntax nodes that can contain a supported boundary.
    pub boundary_candidate_count: usize,
}

/// Error produced when a mandatory Tree-sitter grammar cannot inspect an input.
#[derive(Debug, Error)]
pub enum SourceSyntaxError {
    /// The selected grammar could not be configured.
    #[error("failed to configure Tree-sitter grammar: {0}")]
    Grammar(#[from] tree_sitter::LanguageError),
    /// Tree-sitter did not return a syntax tree.
    #[error("Tree-sitter did not return a syntax tree")]
    MissingTree,
}

/// Inspects a focused source file with its mandatory Tree-sitter grammar.
///
/// The resulting candidate count is used as a structural guard for the narrower
/// framework recognizers; it does not create or persist a repository-local AST.
///
/// # Errors
///
/// Returns [`SourceSyntaxError`] if the grammar cannot be selected or parsing fails.
pub fn inspect_source_syntax(
    language: SourceSyntaxLanguage,
    source_path: &str,
    source: &str,
) -> Result<SourceSyntaxInspection, SourceSyntaxError> {
    let grammar = grammar(language, source_path);
    let mut parser = Parser::new();
    parser.set_language(&grammar)?;
    let tree = parser
        .parse(source, None)
        .ok_or(SourceSyntaxError::MissingTree)?;
    let root = tree.root_node();
    let mut boundary_candidate_count = 0;
    count_candidates(language, root, &mut boundary_candidate_count);
    Ok(SourceSyntaxInspection {
        has_error: root.has_error(),
        boundary_candidate_count,
    })
}

fn grammar(language: SourceSyntaxLanguage, source_path: &str) -> Language {
    match language {
        SourceSyntaxLanguage::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        SourceSyntaxLanguage::TypeScript
            if std::path::Path::new(source_path)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tsx")) =>
        {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        }
        SourceSyntaxLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        SourceSyntaxLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
        SourceSyntaxLanguage::Python => tree_sitter_python::LANGUAGE.into(),
        SourceSyntaxLanguage::Go => tree_sitter_go::LANGUAGE.into(),
        SourceSyntaxLanguage::Java => tree_sitter_java::LANGUAGE.into(),
    }
}

fn count_candidates(
    language: SourceSyntaxLanguage,
    node: tree_sitter::Node<'_>,
    count: &mut usize,
) {
    if candidate_kind(language, node.kind()) {
        *count = count.saturating_add(1);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count_candidates(language, child, count);
    }
}

fn candidate_kind(language: SourceSyntaxLanguage, kind: &str) -> bool {
    match language {
        SourceSyntaxLanguage::JavaScript | SourceSyntaxLanguage::TypeScript => {
            matches!(
                kind,
                "call_expression"
                    | "decorator"
                    | "method_definition"
                    | "function_declaration"
                    | "lexical_declaration"
            )
        }
        SourceSyntaxLanguage::Rust => matches!(kind, "call_expression" | "attribute_item"),
        SourceSyntaxLanguage::Python => {
            matches!(
                kind,
                "call" | "decorator" | "dictionary" | "class_definition"
            )
        }
        SourceSyntaxLanguage::Go => kind == "call_expression",
        SourceSyntaxLanguage::Java => {
            matches!(
                kind,
                "method_invocation" | "annotation" | "marker_annotation"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceSyntaxLanguage, inspect_source_syntax};

    #[test]
    fn mandatory_grammars_should_find_boundary_candidates() {
        let fixtures = [
            (
                SourceSyntaxLanguage::JavaScript,
                "client.js",
                "fetch('/orders');",
            ),
            (
                SourceSyntaxLanguage::TypeScript,
                "router.ts",
                "app.post('/orders', handler);",
            ),
            (
                SourceSyntaxLanguage::JavaScript,
                "src/app/api/orders/route.js",
                "export async function POST() {}",
            ),
            (
                SourceSyntaxLanguage::Rust,
                "lib.rs",
                "#[tokio::test]\nasync fn test_order() {}",
            ),
            (
                SourceSyntaxLanguage::Python,
                "test_api.py",
                "@app.post('/orders')\ndef create(): pass",
            ),
            (
                SourceSyntaxLanguage::Python,
                "methods_config.py",
                "METHODS = {'get_order': ['GET', '/orders/{id}']}",
            ),
            (
                SourceSyntaxLanguage::Python,
                "factories.py",
                "class OrderFactory(factory.Factory):\n class Meta:\n  model = Order",
            ),
            (
                SourceSyntaxLanguage::Go,
                "main.go",
                "package main\nfunc main() { http.Get(\"/orders\") }",
            ),
            (
                SourceSyntaxLanguage::Java,
                "Client.java",
                "class Client { void run() { client.get(); } }",
            ),
        ];

        for (language, path, source) in fixtures {
            let result = inspect_source_syntax(language, path, source);
            assert!(
                matches!(result, Ok(report) if report.boundary_candidate_count > 0),
                "{path}: {result:?}"
            );
        }
    }
}
