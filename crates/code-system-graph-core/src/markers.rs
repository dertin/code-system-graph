//! Exact ASCII case-insensitive marker prefilters for lexical extractors.

#[cfg(test)]
use std::path::{Path, PathBuf};

use aho_corasick::AhoCorasick;

#[cfg(test)]
use crate::SourceLanguage;

/// Multi-pattern substring matcher used to skip inputs that cannot produce observations.
pub(crate) struct MarkerSet {
    matcher: Option<AhoCorasick>,
}

impl MarkerSet {
    pub(crate) fn new(markers: &[&str]) -> Self {
        Self {
            matcher: AhoCorasick::builder()
                .ascii_case_insensitive(true)
                .build(markers)
                .ok(),
        }
    }

    /// Returns whether any marker occurs in `haystack`.
    ///
    /// A matcher that failed to build never filters, so a construction error cannot drop facts.
    pub(crate) fn any_in(&self, haystack: &str) -> bool {
        self.matcher
            .as_ref()
            .is_none_or(|matcher| matcher.is_match(haystack))
    }
}

/// Source files used by prefilter differential tests: repository fixtures plus this workspace's
/// own crates, whose test modules embed snippets for every supported language.
#[cfg(test)]
pub(crate) fn differential_corpus() -> Vec<(SourceLanguage, String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    visit(&root.join("fixtures"), &mut files);
    visit(&root.join("crates"), &mut files);
    files
        .into_iter()
        .filter_map(|path| {
            let language = match path.extension().and_then(|extension| extension.to_str())? {
                "py" => SourceLanguage::Python,
                "js" | "mjs" | "cjs" | "jsx" => SourceLanguage::JavaScript,
                "ts" | "tsx" | "mts" | "cts" => SourceLanguage::TypeScript,
                "go" => SourceLanguage::Go,
                "java" => SourceLanguage::Java,
                "rs" => SourceLanguage::Rust,
                _ => return None,
            };
            let input = std::fs::read_to_string(&path).ok()?;
            Some((language, path.display().to_string(), input))
        })
        .collect()
}

#[cfg(test)]
fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if !matches!(name, "target" | "node_modules" | ".git") {
                visit(&path, files);
            }
        } else {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_set_should_match_ascii_case_insensitively() {
        let markers = MarkerSet::new(&["publish", "pull("]);

        assert!(markers.any_in("client.PUBLISH(topic)"));
        assert!(markers.any_in("sub.Pull(ctx)"));
        assert!(!markers.any_in("fn handler() {}"));
    }
}
