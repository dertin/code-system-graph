use std::ops::ControlFlow;

use thiserror::Error;
use tree_sitter::{Language, ParseOptions, Parser};

use crate::{ExtractionLimitExceeded, ExtractionTracker};

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
    /// Parsing or traversal exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] ExtractionLimitExceeded),
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
    tracker: &mut ExtractionTracker,
) -> Result<SourceSyntaxInspection, SourceSyntaxError> {
    let grammar = grammar(language, source_path);
    let mut parser = Parser::new();
    parser.set_language(&grammar)?;
    let mut timeout = None;
    let tree = {
        let mut progress = |_: &tree_sitter::ParseState| match tracker.check_tree_sitter_time() {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => {
                timeout = Some(error);
                ControlFlow::Break(())
            }
        };
        let options = ParseOptions::new().progress_callback(&mut progress);
        let bytes = source.as_bytes();
        parser.parse_with_options(
            &mut |offset, _| bytes.get(offset..).unwrap_or_default(),
            None,
            Some(options),
        )
    };
    if let Some(error) = timeout {
        return Err(error.into());
    }
    let tree = tree.ok_or(SourceSyntaxError::MissingTree)?;
    let root = tree.root_node();
    let mut boundary_candidate_count = 0;
    count_candidates(language, root, &mut boundary_candidate_count, tracker)?;
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
    root: tree_sitter::Node<'_>,
    count: &mut usize,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.charge_tree_sitter_node(0)?;
    let mut pending = vec![(root, 0_u64)];
    while let Some((node, depth)) = pending.pop() {
        if candidate_kind(language, node.kind()) {
            *count = count.saturating_add(1);
        }
        let child_depth = depth.saturating_add(1);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            tracker.charge_tree_sitter_node(child_depth)?;
            pending.push((child, child_depth));
        }
    }
    Ok(())
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
    use super::{SourceSyntaxError, SourceSyntaxLanguage, inspect_source_syntax};
    use crate::{ExtractionBudgets, ExtractionResource, ExtractionTracker};

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
            let mut tracker =
                ExtractionTracker::new(path, "tree-sitter", &ExtractionBudgets::default());
            let result = inspect_source_syntax(language, path, source, &mut tracker);
            assert!(
                matches!(result, Ok(report) if report.boundary_candidate_count > 0),
                "{path}: {result:?}"
            );
        }
    }

    #[test]
    fn javascript_ast_depth_should_accept_256_and_reject_257() {
        fn nested_arrays(count: usize) -> String {
            format!("{}0{};", "[".repeat(count), "]".repeat(count))
        }

        let budgets = ExtractionBudgets {
            max_ast_depth_per_artifact: 256,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("exact.js", "tree-sitter", &budgets);
        let mut above = ExtractionTracker::new("above.js", "tree-sitter", &budgets);

        assert!(
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "exact.js",
                &nested_arrays(254),
                &mut exact,
            )
            .is_ok()
        );
        assert!(matches!(
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "above.js",
                &nested_arrays(255),
                &mut above,
            ),
            Err(SourceSyntaxError::LimitExceeded(error))
                if error.resource == ExtractionResource::AstDepth
                    && error.observed == 257
                    && error.maximum == 256
        ));
    }

    #[test]
    fn javascript_node_budget_should_accept_500000_and_reject_500001() {
        let budgets = ExtractionBudgets {
            max_tree_sitter_nodes_per_artifact: 500_000,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("exact.js", "tree-sitter", &budgets);
        let mut above = ExtractionTracker::new("above.js", "tree-sitter", &budgets);
        let exact_source = format!("{}// one", ";".repeat(249_999));
        let above_source = ";".repeat(250_000);

        assert!(
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "exact.js",
                &exact_source,
                &mut exact,
            )
            .is_ok()
        );
        assert!(matches!(
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "above.js",
                &above_source,
                &mut above,
            ),
            Err(SourceSyntaxError::LimitExceeded(error))
                if error.resource == ExtractionResource::TreeSitterNodes
                    && error.observed == 500_001
                    && error.maximum == 500_000
        ));
    }

    #[test]
    fn parser_should_cancel_on_a_real_wall_time_deadline() {
        let budgets = ExtractionBudgets {
            max_tree_sitter_wall_time_ms_per_artifact: 1,
            max_tree_sitter_nodes_per_artifact: 1_000_000,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("slow.js", "tree-sitter", &budgets);
        let source = ";".repeat(750_000);

        assert!(matches!(
            inspect_source_syntax(
                SourceSyntaxLanguage::JavaScript,
                "slow.js",
                &source,
                &mut tracker,
            ),
            Err(SourceSyntaxError::LimitExceeded(error))
                if error.resource == ExtractionResource::TreeSitterWallTimeMs
        ));
    }
}
