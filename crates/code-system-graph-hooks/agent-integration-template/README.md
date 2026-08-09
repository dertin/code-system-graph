# Agent integration templates

This directory is the single source of truth for editable agent installation, skill, discovery,
and activation content. Rust code may select, validate, merge, quote, and render these files, but
must not duplicate their editable prose, classifier vocabulary, or shell bodies. MCP tool schemas,
runtime results, validation errors, and protocol adapters remain next to the Rust API they define.

## Layout

- `agent-plugin/` contains the Agent Plugins 1.0 portable package. Its
  `skills/code-system-graph/SKILL.md` is the only detailed MCP operating procedure. Its
  `metadata/` directory owns discovery and optional client-facing labels rendered into that skill.
- `native-hooks/` contains optional activation adapters: prompt signals, short selector guidance,
  static fallback rules, strict-gate shell, host limitations, and UI text. These files may select
  the canonical skill but must not copy its detailed workflow.

Generated plugin repositories such as `hugint-agent-plugin` contain managed rendered copies, not
another source. After changing this directory, rebuild `csgraph` and `code-system-graph-hooks`.
When an existing rendered skill changes, run `csgraph plugin uninstall` for that managed
integration and then run the plugin installer to render it again. `--replace-generated` updates
owned local binding state; it does not overwrite a different versioned skill.

## Placeholder contracts

Agent Plugin templates are rendered by the strict CLI renderer. Unknown, missing, duplicated, or
unused `{{NAME}}` placeholders fail generation.

- `agent-plugin/plugin.json`: `PLUGIN_NAME`, `PLUGIN_VERSION`, `WORKSPACE_JSON_STRING`;
- `agent-plugin/mcp.json`: `MCP_SERVER_NAME`;
- `agent-plugin/skills/code-system-graph/SKILL.md`: `SKILL_NAME`,
  `SKILL_DESCRIPTION_YAML`, `WORKSPACE`;
- `agent-plugin/skills/code-system-graph/agents/openai.yaml`: `SKILL_DISPLAY_NAME_YAML`,
  `SKILL_DEFAULT_PROMPT_YAML`;
- `agent-plugin/metadata/skill-description.txt`: `WORKSPACE`;
- `agent-plugin/metadata/openai-display-name.txt`: `WORKSPACE`;
- `agent-plugin/metadata/openai-default-prompt.txt`: `SKILL_NAME`.

Native templates use these exact placeholders:

- `static-rule.md`: `BEGIN_MARKER`, `ROUTING`, `END_MARKER`;
- `cursor-rule.mdc`: `PRODUCT_MARKER`, `BLOCK`;
- `strict-gate.sh`: `BEGIN_MARKER`, `BINARY`, `DATABASE`, `WORKSPACE`, `REPOSITORY`, `END_MARKER`.

Dynamic guidance, signal lists, limitation text, and metadata contain no placeholders. Run
`cargo test -p code-system-graph-hooks --all-features --locked` and the Agent Plugin E2E suite
after editing this tree.

Signal matching is deterministic: the prompt is lowercased and every nonempty signal line is
matched as a literal substring, not by a semantic or language-independent classifier. Write signal
lines in lowercase, keep phrases specific enough to express graph intent, include accented and
unaccented variants when needed, and add positive and negative tests for each supported language.
The bundled lists cover high-precision English and Spanish phrases; other languages may not
activate a dynamic hook. Avoid broad fragments such as `where is`, `dónde`, `code`, or `test` even
in a programming-only host.
