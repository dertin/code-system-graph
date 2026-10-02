//! Canonical persisted identity for source-level symbols.

use code_system_graph_model::{Node, NodeId, NodeKind, RepoId, stable_id};

/// Typed identity shared by source, data, corroboration, and Explore projections.
///
/// `language` is absent only for a data-only symbol whose source parser did not emit a language-
/// specific symbol. Callers compare [`Self::location_key`] when they need to join those two views.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceSymbolIdentity {
    repository: RepoId,
    language: Option<String>,
    source_path: String,
    symbol: String,
}

impl SourceSymbolIdentity {
    /// Creates a language-specific source symbol.
    #[must_use]
    pub fn new(
        repository: RepoId,
        language: impl Into<String>,
        source_path: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            language: Some(language.into().trim().to_ascii_lowercase()),
            source_path: source_path.into(),
            symbol: symbol.into(),
        }
    }

    /// Creates a data-only source symbol when no language-specific node exists.
    #[must_use]
    pub fn data(
        repository: RepoId,
        source_path: impl Into<String>,
        symbol: impl Into<String>,
    ) -> Self {
        Self {
            repository,
            language: None,
            source_path: source_path.into(),
            symbol: symbol.into(),
        }
    }

    /// Parses either canonical persisted symbol representation without consulting its display label.
    #[must_use]
    pub fn from_node(node: &Node) -> Option<Self> {
        if node.kind != NodeKind::SymbolRef {
            return None;
        }
        let repository = node.repo_id.clone()?;
        let source_prefix = format!("symbol:{}:", repository.as_str());
        if let Some(remainder) = node.stable_key.strip_prefix(&source_prefix) {
            let (language, source_identity) = remainder.split_once(':')?;
            let (source_path, symbol) = source_identity.rsplit_once(':')?;
            if language.is_empty() || source_path.is_empty() || symbol.is_empty() {
                return None;
            }
            return Some(Self::new(
                repository,
                decode_component(language)?,
                decode_component(source_path)?,
                decode_component(symbol)?,
            ));
        }
        let data_prefix = format!("data-symbol:{}:", repository.as_str());
        let source_identity = node.stable_key.strip_prefix(&data_prefix)?;
        let (source_path, symbol) = source_identity.rsplit_once(':')?;
        if source_path.is_empty() || symbol.is_empty() {
            return None;
        }
        Some(Self::data(
            repository,
            decode_component(source_path)?,
            decode_component(symbol)?,
        ))
    }

    /// Repository owning the source artifact.
    #[must_use]
    pub const fn repository(&self) -> &RepoId {
        &self.repository
    }

    /// Normalized source language, absent for data-only fallback identities.
    #[must_use]
    pub fn language(&self) -> Option<&str> {
        self.language.as_deref()
    }

    /// Portable repository-relative source path.
    #[must_use]
    pub fn source_path(&self) -> &str {
        &self.source_path
    }

    /// Unqualified symbol name observed by the extractor.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Stable comparison key that deliberately ignores the optional language dimension.
    #[must_use]
    pub fn location_key(&self) -> (&RepoId, &str, &str) {
        (&self.repository, &self.source_path, &self.symbol)
    }

    /// Canonical persisted key.
    #[must_use]
    pub fn stable_key(&self) -> String {
        self.language.as_ref().map_or_else(
            || {
                format!(
                    "data-symbol:{}:{}:{}",
                    self.repository.as_str(),
                    encode_component(&self.source_path),
                    encode_component(&self.symbol)
                )
            },
            |language| {
                format!(
                    "symbol:{}:{}:{}:{}",
                    self.repository.as_str(),
                    encode_component(language),
                    encode_component(&self.source_path),
                    encode_component(&self.symbol)
                )
            },
        )
    }

    /// Stable node identifier derived from the canonical key.
    #[must_use]
    pub fn node_id(&self) -> NodeId {
        NodeId::new(stable_id("node", &self.stable_key()))
    }

    /// Creates the persisted graph node while keeping display text separate from identity.
    #[must_use]
    pub fn node(&self, label: impl Into<String>) -> Node {
        Node {
            id: self.node_id(),
            kind: NodeKind::SymbolRef,
            repo_id: Some(self.repository.clone()),
            stable_key: self.stable_key(),
            label: label.into(),
        }
    }
}

fn encode_component(value: &str) -> String {
    value.replace('%', "%25").replace(':', "%3A")
}

fn decode_component(value: &str) -> Option<String> {
    let mut remaining = value;
    let mut decoded = String::with_capacity(value.len());
    while let Some(index) = remaining.find('%') {
        decoded.push_str(&remaining[..index]);
        remaining = &remaining[index..];
        if let Some(rest) = remaining.strip_prefix("%25") {
            decoded.push('%');
            remaining = rest;
        } else {
            let rest = remaining.strip_prefix("%3A")?;
            decoded.push(':');
            remaining = rest;
        }
    }
    decoded.push_str(remaining);
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{NodeKind, RepoId};

    use super::SourceSymbolIdentity;

    #[test]
    fn every_supported_language_round_trips_without_using_the_label() {
        for language in ["javascript", "typescript", "rust", "python", "go", "java"] {
            let identity = SourceSymbolIdentity::new(
                RepoId::new("repo:api"),
                language,
                "src/nested:module/file.rs",
                "load_orders",
            );
            let mut node = identity.node("an unrelated display label");
            node.label = "label changes must be harmless".to_owned();

            assert_eq!(SourceSymbolIdentity::from_node(&node), Some(identity));
        }
    }

    #[test]
    fn qualified_symbol_and_delimiter_characters_round_trip_without_ambiguity() {
        let identity = SourceSymbolIdentity::new(
            RepoId::new("repo:api"),
            "rust:async",
            "src/nested:module/percent%file.rs",
            "CaféClient::get%checked",
        );

        assert_eq!(
            SourceSymbolIdentity::from_node(&identity.node("unrelated")),
            Some(identity)
        );
    }

    #[test]
    fn data_only_identity_round_trips_and_shares_the_source_location() {
        let source =
            SourceSymbolIdentity::new(RepoId::new("repo:api"), "rust", "src/lib.rs", "list_orders");
        let data = SourceSymbolIdentity::data(RepoId::new("repo:api"), "src/lib.rs", "list_orders");

        assert_eq!(source.location_key(), data.location_key());
        assert_eq!(
            SourceSymbolIdentity::from_node(&data.node("anything")),
            Some(data)
        );
    }

    #[test]
    fn non_symbol_nodes_are_not_parsed_as_source_identities() {
        let mut node =
            SourceSymbolIdentity::data(RepoId::new("repo:api"), "src/lib.rs", "list_orders")
                .node("list_orders");
        node.kind = NodeKind::Artifact;

        assert!(SourceSymbolIdentity::from_node(&node).is_none());
    }
}
