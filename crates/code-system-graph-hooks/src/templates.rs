//! Visible, versioned templates shared by Agent Plugin generation and native host adapters.

/// Portable Agent Plugins 1.0 manifest template.
pub const AGENT_PLUGIN_MANIFEST: &str =
    include_str!("../agent-integration-template/agent-plugin/plugin.json");
/// Portable Agent Plugins 1.0 MCP template.
pub const AGENT_PLUGIN_MCP: &str =
    include_str!("../agent-integration-template/agent-plugin/mcp.json");
/// Canonical cross-client Code System Graph skill.
pub const AGENT_PLUGIN_SKILL: &str =
    include_str!("../agent-integration-template/agent-plugin/skills/code-system-graph/SKILL.md");
/// Optional Codex UI metadata for the canonical skill.
pub const AGENT_PLUGIN_OPENAI_METADATA: &str = include_str!(
    "../agent-integration-template/agent-plugin/skills/code-system-graph/agents/openai.yaml"
);
/// Extended operating guide referenced by the canonical skill.
pub const AGENT_PLUGIN_OPERATING_GUIDE: &str = include_str!(
    "../agent-integration-template/agent-plugin/skills/code-system-graph/references/operating-guide.md"
);
/// License emitted in a complete portable plugin.
pub const AGENT_PLUGIN_LICENSE: &str =
    include_str!("../agent-integration-template/agent-plugin/LICENSE");
/// Ignore rule emitted for developer-local plugin bindings.
pub const AGENT_PLUGIN_GITIGNORE: &str =
    include_str!("../agent-integration-template/agent-plugin/generated.gitignore");
/// Skill discovery description rendered into the portable frontmatter.
pub const AGENT_PLUGIN_SKILL_DESCRIPTION: &str =
    include_str!("../agent-integration-template/agent-plugin/metadata/skill-description.txt");
/// Optional Codex display name for the canonical skill.
pub const AGENT_PLUGIN_OPENAI_DISPLAY_NAME: &str =
    include_str!("../agent-integration-template/agent-plugin/metadata/openai-display-name.txt");
/// Optional Codex starter prompt for the canonical skill.
pub const AGENT_PLUGIN_OPENAI_DEFAULT_PROMPT: &str =
    include_str!("../agent-integration-template/agent-plugin/metadata/openai-default-prompt.txt");

/// Dynamic prompt-hook guidance for federated work with `CodeGraph` enrichment.
pub const FEDERATED_CODEGRAPH_GUIDANCE: &str =
    include_str!("../agent-integration-template/native-hooks/guidance/federated-codegraph.txt");
/// Dynamic prompt-hook guidance for repository-local work with `CodeGraph` enrichment.
pub const LOCAL_CODEGRAPH_GUIDANCE: &str =
    include_str!("../agent-integration-template/native-hooks/guidance/local-codegraph.txt");
/// Dynamic prompt-hook guidance for federated work without `CodeGraph` enrichment.
pub const FEDERATED_NATIVE_GUIDANCE: &str =
    include_str!("../agent-integration-template/native-hooks/guidance/federated-native.txt");
/// Dynamic prompt-hook guidance for repository-local work without `CodeGraph` enrichment.
pub const LOCAL_NATIVE_GUIDANCE: &str =
    include_str!("../agent-integration-template/native-hooks/guidance/local-native.txt");
/// Newline-separated prompt signals for federated intent.
pub const FEDERATED_SIGNALS: &str =
    include_str!("../agent-integration-template/native-hooks/signals/federated.txt");
/// Newline-separated prompt signals for repository-local intent.
pub const LOCAL_SIGNALS: &str =
    include_str!("../agent-integration-template/native-hooks/signals/local.txt");
/// Marker-scoped static-rule template used only as a compatibility fallback.
pub const STATIC_RULE: &str =
    include_str!("../agent-integration-template/native-hooks/static-rule.md");
/// Static routing policy when `CodeGraph` enrichment is enabled.
pub const STATIC_ROUTING_CODEGRAPH: &str =
    include_str!("../agent-integration-template/native-hooks/static-routing-codegraph.md");
/// Static routing policy when `CodeGraph` enrichment is unavailable.
pub const STATIC_ROUTING_NATIVE: &str =
    include_str!("../agent-integration-template/native-hooks/static-routing-native.md");
/// Cursor wrapper for the static compatibility rule.
pub const CURSOR_RULE: &str =
    include_str!("../agent-integration-template/native-hooks/cursor-rule.mdc");
/// Strict pre-commit gate shell template.
pub const STRICT_GATE: &str =
    include_str!("../agent-integration-template/native-hooks/strict-gate.sh");
/// Cursor limitation reported by hook installation.
pub const CURSOR_LIMITATION: &str =
    include_str!("../agent-integration-template/native-hooks/limitations/cursor.txt");
/// Antigravity limitation reported by hook installation.
pub const ANTIGRAVITY_LIMITATION: &str =
    include_str!("../agent-integration-template/native-hooks/limitations/antigravity.txt");
/// Claude/Codex hook status text.
pub const HOOK_STATUS_MESSAGE: &str =
    include_str!("../agent-integration-template/native-hooks/metadata/status-message.txt");
/// Gemini hook description.
pub const GEMINI_HOOK_DESCRIPTION: &str =
    include_str!("../agent-integration-template/native-hooks/metadata/gemini-description.txt");

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        AGENT_PLUGIN_MANIFEST, AGENT_PLUGIN_MCP, AGENT_PLUGIN_OPENAI_DEFAULT_PROMPT, AGENT_PLUGIN_OPENAI_DISPLAY_NAME, AGENT_PLUGIN_OPENAI_METADATA, AGENT_PLUGIN_SKILL, AGENT_PLUGIN_SKILL_DESCRIPTION, CURSOR_RULE, FEDERATED_CODEGRAPH_GUIDANCE, FEDERATED_NATIVE_GUIDANCE, FEDERATED_SIGNALS, GEMINI_HOOK_DESCRIPTION, HOOK_STATUS_MESSAGE, LOCAL_CODEGRAPH_GUIDANCE, LOCAL_NATIVE_GUIDANCE, LOCAL_SIGNALS, STATIC_RULE, STRICT_GATE
    };

    #[test]
    fn template_placeholder_contracts_should_be_explicit() {
        assert_eq!(
            placeholders(AGENT_PLUGIN_MANIFEST),
            set(&["PLUGIN_NAME", "PLUGIN_VERSION", "WORKSPACE_JSON_STRING"])
        );
        assert_eq!(placeholders(AGENT_PLUGIN_MCP), set(&["MCP_SERVER_NAME"]));
        assert_eq!(
            placeholders(AGENT_PLUGIN_SKILL),
            set(&["SKILL_DESCRIPTION_YAML", "SKILL_NAME", "WORKSPACE"])
        );
        assert_eq!(
            placeholders(AGENT_PLUGIN_OPENAI_METADATA),
            set(&["SKILL_DEFAULT_PROMPT_YAML", "SKILL_DISPLAY_NAME_YAML"])
        );
        assert_eq!(
            placeholders(STATIC_RULE),
            set(&["BEGIN_MARKER", "END_MARKER", "ROUTING"])
        );
        assert_eq!(placeholders(CURSOR_RULE), set(&["BLOCK", "PRODUCT_MARKER"]));
        assert_eq!(
            placeholders(AGENT_PLUGIN_SKILL_DESCRIPTION),
            set(&["WORKSPACE"])
        );
        assert_eq!(
            placeholders(AGENT_PLUGIN_OPENAI_DISPLAY_NAME),
            set(&["WORKSPACE"])
        );
        assert_eq!(
            placeholders(AGENT_PLUGIN_OPENAI_DEFAULT_PROMPT),
            set(&["SKILL_NAME"])
        );
        assert_eq!(
            placeholders(STRICT_GATE),
            set(&[
                "BEGIN_MARKER",
                "BINARY",
                "DATABASE",
                "END_MARKER",
                "REPOSITORY",
                "WORKSPACE",
            ])
        );
        for template in [
            FEDERATED_CODEGRAPH_GUIDANCE,
            FEDERATED_NATIVE_GUIDANCE,
            LOCAL_CODEGRAPH_GUIDANCE,
            LOCAL_NATIVE_GUIDANCE,
            HOOK_STATUS_MESSAGE,
            GEMINI_HOOK_DESCRIPTION,
        ] {
            assert_ne!(template.trim(), "");
            assert!(placeholders(template).is_empty());
        }
    }

    #[test]
    fn routing_signals_should_be_lowercase_specific_and_unique() {
        let forbidden = set(&[
            "architecture",
            "code",
            "diff",
            "donde",
            "dónde",
            "impact",
            "pull request",
            "repo",
            "repository",
            "test",
            "where does",
            "where is",
        ]);
        for signals in [FEDERATED_SIGNALS, LOCAL_SIGNALS] {
            let mut observed = BTreeSet::new();
            for signal in signals.lines().map(str::trim) {
                assert_ne!(signal, "");
                assert_eq!(signal, signal.to_lowercase());
                assert!(
                    observed.insert(signal),
                    "duplicate routing signal `{signal}`"
                );
                assert!(
                    !forbidden.contains(signal),
                    "routing signal `{signal}` is too broad"
                );
            }
        }
    }

    fn placeholders(template: &str) -> BTreeSet<&str> {
        template
            .split("{{")
            .skip(1)
            .map(|remainder| {
                remainder
                    .split_once("}}")
                    .map(|(name, _)| name)
                    .expect("every template placeholder must close")
            })
            .collect()
    }

    fn set<'a>(values: &[&'a str]) -> BTreeSet<&'a str> {
        values.iter().copied().collect()
    }
}
